//! JPEG: the segments from SOI to the first scan, and the rewrite of that header.

use std::ops::Range;

use super::{
    Container, EXIF_PREFIX, Raw, Walk, Want, XMP_SIG, be16, be32, capped_copy, out_vec, splice_one,
    xmp_span, zeroed,
};
use crate::budget::Budget;
use crate::rewrite::{MIN_EXIF_LEN, kept_exif, reset_exif_in_place};
use crate::{Error, Keep, Limit, Limits};

const EXT_SIG: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";
const ICC_SIG: &[u8] = b"ICC_PROFILE\0";

/// APP1 length of the minimal EXIF: length field, `Exif\0\0`, the TIFF block.
const MIN_APP1_LEN: u8 = 2 + 6 + 26;
const _: () = assert!(MIN_EXIF_LEN == 26);

/// One marker segment: `start` is its first `FF`, `pstart..end` its payload.
#[derive(Clone, Copy)]
struct Seg {
    marker: u8,
    start: usize,
    pstart: usize,
    end: usize,
}

impl Seg {
    fn payload<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        data.get(self.pstart..self.end).unwrap_or_default()
    }
}

enum Item {
    Seg(Seg),
    /// The first `FF DA`, at this offset.
    Scan(usize),
    /// `FF D9`, at this offset.
    Eoi,
    /// The data ends inside the header.
    Trunc,
    /// A byte that is not a marker.
    Bad,
}

struct Segments<'a, 'b> {
    data: &'a [u8],
    pos: usize,
    walk: Walk<'a, 'b>,
}

impl<'a, 'b> Segments<'a, 'b> {
    fn new(data: &'a [u8], limits: &'a Limits, budget: &'a Budget<'b>) -> Self {
        Self {
            data,
            pos: 2,
            walk: Walk::new(limits, budget),
        }
    }

    fn step(&mut self) -> Result<Item, Error> {
        self.walk.tick()?;
        let d = self.data;
        let start = self.pos;
        if d.get(start) != Some(&0xFF) {
            return Ok(if start >= d.len() {
                Item::Trunc
            } else {
                Item::Bad
            });
        }
        let mut q = start + 1;
        while d.get(q) == Some(&0xFF) {
            q += 1;
        }
        let Some(&marker) = d.get(q) else {
            return Ok(Item::Trunc);
        };
        q += 1;
        match marker {
            0x00 => Ok(Item::Bad),
            0xD9 => Ok(Item::Eoi),
            0xDA => Ok(Item::Scan(start)),
            0x01 | 0xD0..=0xD8 => {
                self.pos = q;
                Ok(Item::Seg(Seg {
                    marker,
                    start,
                    pstart: q,
                    end: q,
                }))
            }
            _ => {
                let Some(len) = be16(d, q) else {
                    return Ok(Item::Trunc);
                };
                let len = usize::from(len);
                if len < 2 {
                    return Ok(Item::Bad);
                }
                let end = q + len;
                if end > d.len() {
                    return Ok(Item::Trunc);
                }
                self.pos = end;
                Ok(Item::Seg(Seg {
                    marker,
                    start,
                    pstart: q + 2,
                    end,
                }))
            }
        }
    }

    /// The offset after the EOI of the scan that starts at `from` (its `FF DA`), skipping the
    /// segments between scans by their length; the data length when there is no EOI.
    fn scan_end(&mut self, from: usize) -> Result<usize, Error> {
        let d = self.data;
        let mut i = from;
        loop {
            let Some(off) = d.get(i..).and_then(|s| s.iter().position(|&b| b == 0xFF)) else {
                return Ok(d.len());
            };
            i += off;
            let Some(&m) = d.get(i + 1) else {
                return Ok(d.len());
            };
            match m {
                0x00 | 0x01 | 0xD0..=0xD8 | 0xFF => i += 1,
                0xD9 => return Ok(i + 2),
                _ => {
                    self.walk.tick()?;
                    let Some(len) = be16(d, i + 2) else {
                        return Ok(d.len());
                    };
                    i += 2 + usize::from(len).max(2);
                }
            }
        }
    }
}

struct Ext<'a> {
    guid: &'a [u8],
    full: usize,
    off: usize,
    data: &'a [u8],
}

/// An extended XMP chunk after its signature: GUID, full length, offset, data.
fn ext_chunk(b: &[u8]) -> Option<Ext<'_>> {
    Some(Ext {
        guid: b.get(..32)?,
        full: be32(b, 32)? as usize,
        off: be32(b, 36)? as usize,
        data: b.get(40..)?,
    })
}

fn skip_ws(p: &[u8], mut j: usize) -> usize {
    while p.get(j).is_some_and(u8::is_ascii_whitespace) {
        j += 1;
    }
    j
}

/// The GUID that follows `HasExtendedXMP` at `j`: `="GUID"` or `>GUID<`.
fn guid_after_key(p: &[u8], j: usize) -> Option<[u8; 32]> {
    let mut j = skip_ws(p, j);
    match *p.get(j)? {
        b'=' => {
            j = skip_ws(p, j + 1);
            if matches!(p.get(j), Some(b'"' | b'\'')) {
                j += 1;
            } else {
                return None;
            }
        }
        b'>' => j += 1,
        _ => return None,
    }
    j = skip_ws(p, j);
    let g: [u8; 32] = p.get(j..j + 32)?.try_into().ok()?;
    g.iter().all(u8::is_ascii_hexdigit).then_some(g)
}

/// The GUID that the standard packet names with `xmpNote:HasExtendedXMP` (found by a byte
/// search, not by parsing the XML).
fn find_guid(p: &[u8]) -> Option<[u8; 32]> {
    const KEY: &[u8] = b"HasExtendedXMP";
    let mut from = 0;
    while let Some(i) = p.get(from..)?.windows(KEY.len()).position(|w| w == KEY) {
        if let Some(g) = guid_after_key(p, from + i + KEY.len()) {
            return Some(g);
        }
        from += i + 1;
    }
    None
}

/// The rest of an APP1 payload that continues the EXIF block of the segment before it:
/// `Exif\0\0` with no TIFF header after it (ExifTool's multi-segment EXIF).
fn exif_continuation(p: &[u8]) -> Option<&[u8]> {
    let b = p.strip_prefix(EXIF_PREFIX)?;
    (!(b.starts_with(b"II*\0") || b.starts_with(b"MM\0*"))).then_some(b)
}

/// Appends a continuation to the EXIF block, charging the capacity it adds.
fn append_exif(
    v: &mut Vec<u8>,
    more: &[u8],
    limits: &Limits,
    budget: &Budget,
) -> Result<(), Error> {
    let need = v
        .len()
        .checked_add(more.len())
        .filter(|&n| n <= limits.max_exif_bytes)
        .ok_or(Error::LimitExceeded(Limit::ExifBytes))?;
    if need > v.capacity() {
        let target = need
            .max(v.capacity().saturating_mul(2))
            .min(limits.max_exif_bytes);
        let add = target - v.capacity();
        budget.charge(add)?;
        if v.try_reserve_exact(target - v.len()).is_err() {
            budget.release(add);
            return Err(Error::OutOfMemory);
        }
    }
    v.extend_from_slice(more);
    Ok(())
}

/// The offset in `data` of the TIFF header of the EXIF block that [`extract`] takes (the first
/// APP1 `Exif\0\0`). `None` when there is none, or when the block continues in the next
/// segment: past the first segment, file offsets no longer match block offsets.
pub(crate) fn exif_start(
    data: &[u8],
    limits: &Limits,
    budget: &Budget,
) -> Result<Option<usize>, Error> {
    let mut segs = Segments::new(data, limits, budget);
    while let Item::Seg(s) = segs.step()? {
        if s.marker == 0xE1 && s.payload(data).starts_with(EXIF_PREFIX) {
            let continued = matches!(
                segs.step()?,
                Item::Seg(n) if n.marker == 0xE1 && exif_continuation(n.payload(data)).is_some()
            );
            return Ok((!continued).then_some(s.pstart + EXIF_PREFIX.len()));
        }
    }
    Ok(None)
}

pub(crate) fn extract(
    data: &[u8],
    limits: &Limits,
    budget: &Budget,
    want: Want,
) -> Result<Raw, Error> {
    let mut raw = Raw {
        container: Container::Jpeg,
        exif: None,
        xmp: None,
        xmp_extended: None,
        icc: None,
    };
    // ICC chunks as ExifTool collects them: the chunk total of the first chunk, then each chunk
    // under its number (0-255; a repeated number is appended) until as many chunks as the total
    // were seen (a total of 0: the first chunk). A chunk with another total ends it: no profile.
    let mut icc_total: Option<u8> = None;
    let mut icc_seen = 0usize;
    let mut icc_done: Option<usize> = None;
    let mut icc_dead = false;
    let mut icc_len = [0usize; 256];
    // The segment before was the EXIF block or a continuation of it.
    let mut exif_open = false;
    let mut segs = Segments::new(data, limits, budget);
    while let Item::Seg(s) = segs.step()? {
        let p = s.payload(data);
        let was_open = std::mem::replace(&mut exif_open, false);
        match s.marker {
            0xE1 => {
                if let Some(b) = p.strip_prefix(EXIF_PREFIX) {
                    if want.exif && raw.exif.is_none() {
                        raw.exif = Some(capped_copy(
                            budget,
                            b,
                            limits.max_exif_bytes,
                            Limit::ExifBytes,
                        )?);
                        exif_open = true;
                    } else if was_open
                        && let Some(more) = exif_continuation(p)
                        && let Some(v) = raw.exif.as_mut()
                    {
                        append_exif(v, more, limits, budget)?;
                        exif_open = true;
                    }
                } else if let Some(b) = p.strip_prefix(XMP_SIG)
                    && want.xmp
                    && raw.xmp.is_none()
                {
                    raw.xmp = Some(capped_copy(
                        budget,
                        b,
                        limits.max_xmp_bytes,
                        Limit::XmpBytes,
                    )?);
                }
            }
            0xE2 if want.icc && icc_done.is_none() && !icc_dead => {
                if let Some([num, tot, rest @ ..]) = p.strip_prefix(ICC_SIG) {
                    let total = *icc_total.get_or_insert(*tot);
                    if *tot != total {
                        icc_dead = true;
                        continue;
                    }
                    if let Some(l) = icc_len.get_mut(usize::from(*num)) {
                        *l = l.saturating_add(rest.len());
                    }
                    icc_seen += 1;
                    if icc_seen >= usize::from(total) {
                        icc_done = Some(icc_seen);
                    }
                }
            }
            _ => {}
        }
    }

    let icc_bytes = icc_len.iter().fold(0usize, |t, &l| t.saturating_add(l));
    if let Some(n) = icc_done
        && icc_bytes > 0
    {
        raw.icc = Some(icc_profile(data, limits, budget, &icc_len, n, icc_bytes)?);
    }
    let guid = if want.xmp {
        raw.xmp.as_deref().and_then(find_guid)
    } else {
        None
    };
    if let Some(g) = guid {
        let std_len = raw.xmp.as_ref().map_or(0, Vec::len);
        raw.xmp_extended = extended_xmp(data, limits, budget, &g, std_len)?;
    }
    Ok(raw)
}

/// The profile of the first `n` ICC chunks, joined in chunk-number order (the chunks of a
/// repeated number in file order); `lens` holds the bytes per number, `total` their sum.
fn icc_profile(
    data: &[u8],
    limits: &Limits,
    budget: &Budget,
    lens: &[usize; 256],
    n: usize,
    total: usize,
) -> Result<Vec<u8>, Error> {
    let mut buf = zeroed(budget, total)?;
    let mut at = [0usize; 256];
    let mut sum = 0usize;
    for (a, &l) in at.iter_mut().zip(lens) {
        *a = sum;
        sum = sum.saturating_add(l);
    }
    let mut left = n;
    let mut segs = Segments::new(data, limits, budget);
    while left > 0
        && let Item::Seg(s) = segs.step()?
    {
        if s.marker != 0xE2 {
            continue;
        }
        let Some([num, _, rest @ ..]) = s.payload(data).strip_prefix(ICC_SIG) else {
            continue;
        };
        left -= 1;
        if let Some(a) = at.get_mut(usize::from(*num))
            && let Some(dst) = buf.get_mut(*a..a.saturating_add(rest.len()))
        {
            dst.copy_from_slice(rest);
            *a = a.saturating_add(rest.len());
        }
    }
    Ok(buf)
}

/// One extended XMP chunk of the wanted GUID: (offset, full length, data range in the file).
type ExtChunk = (usize, usize, usize, usize);

fn ext_of(s: &Seg, data: &[u8], guid: &[u8; 32]) -> Option<ExtChunk> {
    let c = ext_chunk(s.payload(data).strip_prefix(EXT_SIG)?)?;
    if c.guid != guid.as_slice() || c.data.is_empty() {
        return None;
    }
    Some((c.off, c.full, s.end - c.data.len(), s.end))
}

/// JPEG extended XMP as ExifTool joins it: the chunks of `guid` (compared exactly) under their
/// offsets, a later chunk at an offset replacing an earlier one, the full length of the last
/// chunk; then from offset 0 each chunk leads to the offset after it. `None` unless that chain
/// ends exactly at the full length ("Incomplete extended XMP": nothing is read).
fn extended_xmp(
    data: &[u8],
    limits: &Limits,
    budget: &Budget,
    guid: &[u8; 32],
    std_len: usize,
) -> Result<Option<Vec<u8>>, Error> {
    let mut n = 0usize;
    let mut segs = Segments::new(data, limits, budget);
    while let Item::Seg(s) = segs.step()? {
        if s.marker == 0xE1 && ext_of(&s, data, guid).is_some() {
            n += 1;
        }
    }
    if n == 0 {
        return Ok(None);
    }
    let charged = n.saturating_mul(size_of::<ExtChunk>());
    let mut chunks = budget.vec::<ExtChunk>(n)?;
    let mut segs = Segments::new(data, limits, budget);
    while let Item::Seg(s) = segs.step()? {
        if s.marker == 0xE1
            && chunks.len() < n
            && let Some(c) = ext_of(&s, data, guid)
        {
            chunks.push(c);
        }
    }
    let full = chunks.last().map_or(0, |c| c.1);
    // By offset, then by file position: the last chunk of an offset is the one that counts.
    chunks.sort_unstable_by_key(|c| (c.0, c.2));
    let at = |off: usize| {
        let i = chunks.partition_point(|c| c.0 <= off);
        i.checked_sub(1)
            .and_then(|i| chunks.get(i))
            .filter(|c| c.0 == off)
            .copied()
    };
    let mut off = 0;
    while off < full
        && let Some(c) = at(off)
    {
        off += c.3 - c.2;
    }
    let result = if off == full && full > 0 {
        if full
            .checked_add(std_len)
            .is_none_or(|t| t > limits.max_xmp_bytes)
        {
            budget.release(charged);
            return Err(Error::LimitExceeded(Limit::XmpBytes));
        }
        let mut buf = budget.vec::<u8>(full)?;
        let mut off = 0;
        while off < full
            && let Some(c) = at(off)
        {
            buf.extend_from_slice(data.get(c.2..c.3).unwrap_or_default());
            off += c.3 - c.2;
        }
        Some(buf)
    } else {
        None
    };
    budget.release(charged);
    Ok(result)
}
fn put_exif(out: &mut Vec<u8>, pending: &mut Option<[u8; MIN_EXIF_LEN]>) {
    if let Some(t) = pending.take() {
        out.extend_from_slice(&[0xFF, 0xE1, 0x00, MIN_APP1_LEN]);
        out.extend_from_slice(EXIF_PREFIX);
        out.extend_from_slice(&t);
    }
}

fn keep_segment(marker: u8, p: &[u8], keep: Keep) -> bool {
    match marker {
        0xE0 => p.starts_with(b"JFIF\0"),
        0xEE => p.starts_with(b"Adobe"),
        0xE2 => keep.icc && p.starts_with(ICC_SIG),
        0xE1 | 0xE3..=0xEF | 0xFE => false,
        _ => true,
    }
}

pub(crate) fn strip(
    data: &[u8],
    keep: Keep,
    limits: &Limits,
    budget: &Budget,
) -> Result<Vec<u8>, Error> {
    let mut pending = if keep.orientation {
        kept_exif(
            extract(data, limits, budget, Want::EXIF)?.exif,
            limits,
            budget,
        )?
    } else {
        None
    };
    let mut out = out_vec(budget, data.len())?;
    out.extend_from_slice(&[0xFF, 0xD8]);
    let mut segs = Segments::new(data, limits, budget);
    let mut first = true;
    loop {
        match segs.step()? {
            Item::Seg(s) => {
                let p = s.payload(data);
                let first_jfif = first && s.marker == 0xE0 && p.starts_with(b"JFIF\0");
                first = false;
                if !first_jfif {
                    put_exif(&mut out, &mut pending);
                }
                if keep_segment(s.marker, p, keep) {
                    out.extend_from_slice(data.get(s.start..s.end).unwrap_or_default());
                }
                if first_jfif {
                    put_exif(&mut out, &mut pending);
                }
            }
            Item::Scan(off) => {
                put_exif(&mut out, &mut pending);
                let end = segs.scan_end(off)?;
                out.extend_from_slice(data.get(off..end).unwrap_or_default());
                return Ok(out);
            }
            Item::Eoi => {
                put_exif(&mut out, &mut pending);
                out.extend_from_slice(&[0xFF, 0xD9]);
                return Ok(out);
            }
            Item::Trunc => return Err(Error::Truncated),
            Item::Bad => return Err(Error::Malformed("jpeg marker")),
        }
    }
}

pub(crate) fn reset(data: &[u8], limits: &Limits, budget: &Budget) -> Result<Vec<u8>, Error> {
    let mut exif: Option<Range<usize>> = None;
    let mut xmp: Option<(Seg, Range<usize>)> = None;
    let mut xmp_seen = false;
    let mut segs = Segments::new(data, limits, budget);
    while let Item::Seg(s) = segs.step()? {
        if s.marker != 0xE1 {
            continue;
        }
        let p = s.payload(data);
        if p.starts_with(EXIF_PREFIX) {
            if exif.is_none() {
                exif = Some(s.pstart + EXIF_PREFIX.len()..s.end);
            }
        } else if let Some(b) = p.strip_prefix(XMP_SIG)
            && !xmp_seen
        {
            xmp_seen = true;
            let base = s.pstart + XMP_SIG.len();
            if let Some(r) = xmp_span(b, limits, budget)? {
                xmp = Some((s, base + r.start..base + r.end));
            }
        }
    }
    let mut out = out_vec(budget, data.len())?;
    out.extend_from_slice(data);
    if let Some(block) = exif.and_then(|r| out.get_mut(r)) {
        reset_exif_in_place(block);
    }
    if let Some((s, span)) = xmp {
        let seg_len = (s.end - s.pstart + 2 + 1)
            .checked_sub(span.len())
            .and_then(|n| u16::try_from(n).ok())
            .ok_or(Error::Malformed("jpeg segment length"))?;
        splice_one(&mut out, span);
        if let Some(d) = out.get_mut(s.pstart - 2..s.pstart) {
            d.copy_from_slice(&seg_len.to_be_bytes());
        }
    }
    Ok(out)
}
