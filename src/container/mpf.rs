//! MPF (CIPA DC-007): the MP Entries of a JPEG APP2 `MPF\0` segment, and their fix-up when a
//! rewrite moves the images they point at.
//!
//! The segment holds a TIFF header and the MP Index IFD. Its MP Entry tag (0xB002, UNDEFINED,
//! 16 bytes per image) points at the entries: attribute, size, offset and two dependent image
//! numbers, in the TIFF byte order. Offsets count from the TIFF header (the byte after `MPF\0`),
//! as libultrahdr, Skia and ExifTool read them; the primary image has offset 0, and its size runs
//! from the file's SOI to its EOI.

use std::ops::Range;

use crate::budget::Budget;
use crate::{Error, Limit};

/// The APP2 signature of MPF.
pub(crate) const MPF_SIG: &[u8] = b"MPF\0";
const MP_ENTRY: u16 = 0xB002;
const UNDEFINED: u16 = 7;
const ENTRY_LEN: usize = 16;

/// Where the MP Entries of a TIFF block are.
struct Entries {
    le: bool,
    /// The offset of the first entry in the block.
    at: usize,
    /// The number of entries.
    n: usize,
}

fn rd16(t: &[u8], at: usize, le: bool) -> Option<u16> {
    let b: [u8; 2] = t.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(if le {
        u16::from_le_bytes(b)
    } else {
        u16::from_be_bytes(b)
    })
}

fn rd32(t: &[u8], at: usize, le: bool) -> Option<u32> {
    let b: [u8; 4] = t.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(if le {
        u32::from_le_bytes(b)
    } else {
        u32::from_be_bytes(b)
    })
}

fn wr32(t: &mut [u8], at: usize, le: bool, v: u32) {
    let b = if le { v.to_le_bytes() } else { v.to_be_bytes() };
    if let Some(d) = at.checked_add(4).and_then(|end| t.get_mut(at..end)) {
        d.copy_from_slice(&b);
    }
}

/// The MP Entries of `t` (the bytes after `MPF\0`): those of the first MP Entry tag of the MP
/// Index IFD, when its type is UNDEFINED, its count a multiple of 16 that is not 0, and its value
/// inside `t`.
fn entries(t: &[u8]) -> Option<Entries> {
    let le = match t.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let ifd = usize::try_from(rd32(t, 4, le)?).ok()?;
    let count = rd16(t, ifd, le)?;
    for k in 0..usize::from(count) {
        let e = ifd.checked_add(2)?.checked_add(k.checked_mul(12)?)?;
        if rd16(t, e, le)? != MP_ENTRY {
            continue;
        }
        let typ = rd16(t, e.checked_add(2)?, le)?;
        let len = usize::try_from(rd32(t, e.checked_add(4)?, le)?).ok()?;
        let at = usize::try_from(rd32(t, e.checked_add(8)?, le)?).ok()?;
        if typ != UNDEFINED
            || len == 0
            || !len.is_multiple_of(ENTRY_LEN)
            || at.checked_add(len)? > t.len()
        {
            return None;
        }
        return Some(Entries {
            le,
            at,
            n: len / ENTRY_LEN,
        });
    }
    None
}

/// The (offset, size) of each MP Entry after the first whose offset is not 0 (offset 0 is the
/// primary image), the offset relative to the TIFF header. Nothing when `t` (the bytes after
/// `MPF\0`) has no valid entry list.
pub(crate) fn images(t: &[u8]) -> impl Iterator<Item = (usize, usize)> {
    let (le, at, n) = entries(t).map_or((false, 0, 0), |e| (e.le, e.at, e.n));
    (1..n).filter_map(move |i| {
        let f = at.checked_add(i.checked_mul(ENTRY_LEN)?)?;
        let size = rd32(t, f.checked_add(4)?, le)?;
        let off = rd32(t, f.checked_add(8)?, le)?;
        (off != 0).then_some((off as usize, size as usize))
    })
}

/// Fixes the MP Entries of `t`, the output copy of the bytes after `MPF\0`, whose TIFF header is
/// at `in_tiff` in the input and at `out_tiff` in the output. The first entry and every entry
/// with offset 0 (the primary image) get the size `primary_len`; every other entry gets the
/// offset of its image in the output, when `map` maps its input offset. A value that does not fit
/// a `u32`, an image `map` does not map and one that would start before `out_tiff` are left as
/// they are. Nothing changes when `t` has no valid entry list.
pub(crate) fn patch(
    t: &mut [u8],
    in_tiff: usize,
    out_tiff: usize,
    primary_len: usize,
    map: &OffsetMap,
) {
    let Some(Entries { le, at, n }) = entries(t) else {
        return;
    };
    for i in 0..n {
        let Some(f) = i.checked_mul(ENTRY_LEN).and_then(|o| at.checked_add(o)) else {
            return;
        };
        let Some(off) = rd32(t, f + 8, le) else {
            return;
        };
        if i == 0 || off == 0 {
            if let Ok(size) = u32::try_from(primary_len) {
                wr32(t, f + 4, le, size);
            }
            continue;
        }
        let moved = in_tiff
            .checked_add(off as usize)
            .and_then(|a| map.get(a))
            .and_then(|o| o.checked_sub(out_tiff))
            .and_then(|o| u32::try_from(o).ok());
        if let Some(new) = moved {
            wr32(t, f + 8, le, new);
        }
    }
}

/// Where the input bytes went in the output: (input start, input end, output start) for each run
/// of bytes copied together, in input order. The capacity it grows by is charged to the budget
/// and given back when the map is dropped.
pub(crate) struct OffsetMap<'a, 'b> {
    runs: Vec<(usize, usize, usize)>,
    charged: usize,
    budget: &'a Budget<'b>,
}

impl<'a, 'b> OffsetMap<'a, 'b> {
    pub(crate) fn new(budget: &'a Budget<'b>) -> Self {
        Self {
            runs: Vec::new(),
            charged: 0,
            budget,
        }
    }

    /// Records that the input bytes `input` were copied to the output at `out`. Runs must come
    /// in input order; a run that continues the one before in both is joined to it.
    pub(crate) fn push(&mut self, input: Range<usize>, out: usize) -> Result<(), Error> {
        if input.is_empty() {
            return Ok(());
        }
        if let Some(last) = self.runs.last_mut()
            && last.1 == input.start
            && last.2.checked_add(last.1 - last.0) == Some(out)
        {
            last.1 = input.end;
            return Ok(());
        }
        if self.runs.len() == self.runs.capacity() {
            let add = self.runs.capacity().max(8);
            let bytes = add
                .checked_mul(size_of::<(usize, usize, usize)>())
                .ok_or(Error::LimitExceeded(Limit::Alloc))?;
            self.budget.charge(bytes)?;
            if self.runs.try_reserve_exact(add).is_err() {
                self.budget.release(bytes);
                return Err(Error::OutOfMemory);
            }
            self.charged = self.charged.saturating_add(bytes);
        }
        self.runs.push((input.start, input.end, out));
        Ok(())
    }

    /// The output offset of the input byte at `at`, when it was copied.
    pub(crate) fn get(&self, at: usize) -> Option<usize> {
        let i = self.runs.partition_point(|r| r.0 <= at).checked_sub(1)?;
        let &(start, end, out) = self.runs.get(i)?;
        if at < end {
            out.checked_add(at - start)
        } else {
            None
        }
    }
}

impl Drop for OffsetMap<'_, '_> {
    fn drop(&mut self) {
        self.budget.release(self.charged);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::Limits;

    /// An MPF TIFF block (the bytes after `MPF\0`) with MPFVersion and MP Entries of (size,
    /// offset); the entries start at 38.
    fn block(le: bool, ents: &[(u32, u32)], count: u32) -> Vec<u8> {
        let w16 = |v: u16| if le { v.to_le_bytes() } else { v.to_be_bytes() };
        let w32 = |v: u32| if le { v.to_le_bytes() } else { v.to_be_bytes() };
        let mut t = Vec::new();
        t.extend_from_slice(if le { b"II*\0" } else { b"MM\0*" });
        t.extend_from_slice(&w32(8));
        t.extend_from_slice(&w16(2));
        t.extend_from_slice(&w16(0xB000));
        t.extend_from_slice(&w16(7));
        t.extend_from_slice(&w32(4));
        t.extend_from_slice(b"0100");
        t.extend_from_slice(&w16(0xB002));
        t.extend_from_slice(&w16(7));
        t.extend_from_slice(&w32(count));
        t.extend_from_slice(&w32(38));
        t.extend_from_slice(&w32(0));
        assert_eq!(t.len(), 38);
        for &(size, off) in ents {
            t.extend_from_slice(&w32(if off == 0 { 0x0003_0000 } else { 0 }));
            t.extend_from_slice(&w32(size));
            t.extend_from_slice(&w32(off));
            t.extend_from_slice(&[0; 4]);
        }
        t
    }

    fn entry(t: &[u8], le: bool, i: usize) -> (u32, u32) {
        let f = 38 + 16 * i;
        (rd32(t, f + 4, le).unwrap(), rd32(t, f + 8, le).unwrap())
    }

    const ENTS: [(u32, u32); 4] = [(5000, 0), (10, 900), (10, 200), (10, 0xFFFF_FF00)];

    #[test]
    fn patch_moves_the_images_after_the_shift() {
        let cancel = AtomicBool::new(false);
        let budget = Budget::new(&Limits::default(), &cancel);
        for le in [false, true] {
            // 20 bytes dropped at 500, after the MPF header at 100 and before the image at 1000.
            let mut map = OffsetMap::new(&budget);
            map.push(0..500, 0).unwrap();
            map.push(520..2000, 500).unwrap();
            let mut t = block(le, &ENTS, 64);
            patch(&mut t, 100, 100, 1234, &map);
            assert_eq!(entry(&t, le, 0), (1234, 0));
            assert_eq!(entry(&t, le, 1), (10, 880));
            // Before the shift: unchanged.
            assert_eq!(entry(&t, le, 2), (10, 200));
            // Points out of the file: left as it is.
            assert_eq!(entry(&t, le, 3), (10, 0xFFFF_FF00));
            let found: Vec<_> = images(&t).collect();
            assert_eq!(found, [(880, 10), (200, 10), (0xFFFF_FF00, 10)]);
        }
    }

    #[test]
    fn patch_with_the_shift_before_the_mpf_header() {
        let cancel = AtomicBool::new(false);
        let budget = Budget::new(&Limits::default(), &cancel);
        for le in [false, true] {
            // 20 bytes dropped at 50: the header and the image move together.
            let mut map = OffsetMap::new(&budget);
            map.push(0..50, 0).unwrap();
            map.push(70..2000, 50).unwrap();
            assert_eq!(map.get(100), Some(80));
            assert_eq!(map.get(60), None);
            assert_eq!(map.get(2000), None);
            let mut t = block(le, &ENTS[..2], 32);
            patch(&mut t, 100, 80, 777, &map);
            assert_eq!(entry(&t, le, 0), (777, 0));
            assert_eq!(entry(&t, le, 1), (10, 900));
        }
    }

    #[test]
    fn bad_entry_lists_change_nothing() {
        let cancel = AtomicBool::new(false);
        let budget = Budget::new(&Limits::default(), &cancel);
        let mut map = OffsetMap::new(&budget);
        map.push(0..10_000, 5).unwrap();
        for le in [false, true] {
            // A count that is not a multiple of 16, a count past the end, no entries.
            for count in [17, 48 + 16, 0] {
                let t = block(le, &ENTS[..3], count);
                let mut p = t.clone();
                patch(&mut p, 0, 0, 1, &map);
                assert_eq!(p, t, "count {count}");
                assert_eq!(images(&t).count(), 0);
            }
            // Cut anywhere: no panic, no change past the cut.
            let t = block(le, &ENTS, 64);
            for n in 0..t.len() {
                let mut p = t[..n].to_vec();
                patch(&mut p, 0, 0, 1, &map);
                let _ = images(&p).count();
            }
        }
        // A byte order that is not II or MM.
        let mut t = block(true, &ENTS, 64);
        t[0] = b'X';
        let p = t.clone();
        patch(&mut t, 0, 0, 1, &map);
        assert_eq!(t, p);
    }

    #[test]
    fn map_joins_runs_and_gives_back_its_charge() {
        let cancel = AtomicBool::new(false);
        let budget = Budget::new(&Limits::default(), &cancel);
        {
            let mut map = OffsetMap::new(&budget);
            map.push(0..10, 0).unwrap();
            map.push(10..20, 10).unwrap();
            map.push(30..40, 20).unwrap();
            map.push(40..40, 99).unwrap();
            assert_eq!(map.runs, [(0, 20, 0), (30, 40, 20)]);
            assert_eq!(map.get(35), Some(25));
            assert!(budget.used() > 0);
        }
        assert_eq!(budget.used(), 0);
    }
}
