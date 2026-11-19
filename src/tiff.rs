//! The bounded IFD walk over a TIFF-structured EXIF block.
//!
//! Layout follows the TIFF 6.0 / EXIF 2.32 structure. ExifTool 13.64 is the behavioural
//! reference (no code is copied): a bad entry or a bad sub-IFD offset is skipped, never an error.

use std::ops::Range;

use crate::budget::Budget;
use crate::exif::{ByteOrder, Entry, Exif, IfdKind, Rational, SRational, Value};
use crate::{Error, Limit, Limits};

const EXIF_PREFIX: &[u8] = b"Exif\0\0";
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_GPS_IFD: u16 = 0x8825;
const TAG_INTEROP_IFD: u16 = 0xA005;
const TAG_THUMB_OFFSET: u16 = 0x0201;
const TAG_THUMB_LENGTH: u16 = 0x0202;
const TAG_ORIENTATION: u16 = 0x0112;
const ENTRY_SIZE: usize = 12;
/// EXIF 3.0 UTF-8 string type, read like ASCII (as ExifTool 13.64 does).
const TYPE_UTF8: u16 = 129;

/// The first `N` bytes of `b`, zero padded when `b` is shorter (callers pass checked slices).
fn arr<const N: usize>(b: &[u8]) -> [u8; N] {
    let mut a = [0u8; N];
    for (d, s) in a.iter_mut().zip(b) {
        *d = *s;
    }
    a
}

/// `b[off..]`, or an empty slice.
fn at(b: &[u8], off: usize) -> &[u8] {
    b.get(off..).unwrap_or_default()
}

fn to_usize(v: u32) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

/// Reads integers in one byte order.
#[derive(Clone, Copy)]
struct Rd(ByteOrder);

impl Rd {
    fn u16(self, b: &[u8]) -> u16 {
        match self.0 {
            ByteOrder::Little => u16::from_le_bytes(arr(b)),
            ByteOrder::Big => u16::from_be_bytes(arr(b)),
        }
    }

    fn u32(self, b: &[u8]) -> u32 {
        match self.0 {
            ByteOrder::Little => u32::from_le_bytes(arr(b)),
            ByteOrder::Big => u32::from_be_bytes(arr(b)),
        }
    }

    fn u64(self, b: &[u8]) -> u64 {
        match self.0 {
            ByteOrder::Little => u64::from_le_bytes(arr(b)),
            ByteOrder::Big => u64::from_be_bytes(arr(b)),
        }
    }
}

/// Skips an optional `Exif\0\0` prefix and checks the TIFF header. Returns the prefix length, the
/// bytes from the TIFF header on, and the byte order. Only the byte order mark is checked: like
/// ExifTool (which only warns), a wrong magic number (not 42) is accepted.
fn header(tiff: &[u8]) -> Result<(usize, &[u8], ByteOrder), Error> {
    let skip = if tiff.starts_with(EXIF_PREFIX) {
        EXIF_PREFIX.len()
    } else {
        0
    };
    let body = at(tiff, skip);
    let order = match body.get(..2) {
        Some(b"II") => ByteOrder::Little,
        Some(b"MM") => ByteOrder::Big,
        _ => return Err(Error::Malformed("tiff header")),
    };
    if body.len() < 8 {
        return Err(Error::Malformed("tiff header"));
    }
    Ok((skip, body, order))
}

fn type_size(typ: u16) -> Option<usize> {
    match typ {
        1 | 2 | 6 | 7 | TYPE_UTF8 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 | 13 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

/// Decodes `data` (exactly `count * type size` bytes) into a value, charging the budget.
fn decode(
    typ: u16,
    data: &[u8],
    count: usize,
    rd: Rd,
    budget: &Budget<'_>,
) -> Result<Option<Value>, Error> {
    Ok(Some(match typ {
        1 => Value::Byte(budget.copy(data)?),
        2 | TYPE_UTF8 => Value::Ascii(budget.copy(data)?),
        7 => Value::Undefined(budget.copy(data)?),
        6 => {
            let mut v = budget.vec::<i8>(count)?;
            v.extend(data.iter().map(|&b| i8::from_ne_bytes([b])));
            Value::SByte(v)
        }
        3 => {
            let mut v = budget.vec::<u16>(count)?;
            v.extend(data.as_chunks::<2>().0.iter().map(|c| rd.u16(c)));
            Value::Short(v)
        }
        8 => {
            let mut v = budget.vec::<i16>(count)?;
            v.extend(
                data.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| i16::from_ne_bytes(rd.u16(c).to_ne_bytes())),
            );
            Value::SShort(v)
        }
        4 | 13 => {
            let mut v = budget.vec::<u32>(count)?;
            v.extend(data.as_chunks::<4>().0.iter().map(|c| rd.u32(c)));
            Value::Long(v)
        }
        9 => {
            let mut v = budget.vec::<i32>(count)?;
            v.extend(
                data.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| i32::from_ne_bytes(rd.u32(c).to_ne_bytes())),
            );
            Value::SLong(v)
        }
        5 => {
            let mut v = budget.vec::<Rational>(count)?;
            v.extend(data.as_chunks::<8>().0.iter().map(|c| Rational {
                num: rd.u32(c),
                den: rd.u32(at(c, 4)),
            }));
            Value::Rational(v)
        }
        10 => {
            let mut v = budget.vec::<SRational>(count)?;
            v.extend(data.as_chunks::<8>().0.iter().map(|c| SRational {
                num: i32::from_ne_bytes(rd.u32(c).to_ne_bytes()),
                den: i32::from_ne_bytes(rd.u32(at(c, 4)).to_ne_bytes()),
            }));
            Value::SRational(v)
        }
        11 => {
            let mut v = budget.vec::<f32>(count)?;
            v.extend(
                data.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| f32::from_bits(rd.u32(c))),
            );
            Value::Float(v)
        }
        12 => {
            let mut v = budget.vec::<f64>(count)?;
            v.extend(
                data.as_chunks::<8>()
                    .0
                    .iter()
                    .map(|c| f64::from_bits(rd.u64(c))),
            );
            Value::Double(v)
        }
        _ => return Ok(None),
    }))
}

/// First value of a SHORT or LONG value.
fn first_uint(value: &Value) -> Option<u32> {
    match value {
        Value::Short(v) => v.first().map(|&x| u32::from(x)),
        Value::Long(v) => v.first().copied(),
        _ => None,
    }
}

struct Walk<'a, 'b> {
    body: &'a [u8],
    /// The file from the TIFF header on (empty when unknown), for a thumbnail past the block.
    file: &'a [u8],
    rd: Rd,
    limits: &'a Limits,
    budget: &'a Budget<'b>,
    ifds: [Vec<Entry>; 5],
    /// The offsets walked, with the kind each was walked as. At most 5 entries (one IFD of each
    /// kind, plus GPS over Interop), so this is not input sized.
    visited: Vec<(usize, IfdKind)>,
    total_entries: usize,
    value_bytes: usize,
    thumbnail: Option<Vec<u8>>,
}

impl Walk<'_, '_> {
    fn add_value_bytes(&mut self, n: usize) -> Result<(), Error> {
        self.value_bytes = self.value_bytes.saturating_add(n);
        if self.value_bytes > self.limits.max_value_bytes {
            return Err(Error::LimitExceeded(Limit::ValueBytes));
        }
        Ok(())
    }

    /// Walks one IFD, then the IFDs it points to in ExifTool's order: each sub-IFD when its
    /// pointer entry is met (depth first, so the Interop IFD inside the Exif IFD), and IFD1
    /// (IFD0's next pointer) after all of them. A bad offset skips the IFD; so does an offset
    /// already walked (the first kind keeps it), except that GPS may reread an Interop IFD (as
    /// ExifTool does, for a wrong Windows Phone pointer).
    fn ifd(&mut self, off: usize, kind: IfdKind, depth: usize) -> Result<(), Error> {
        let dir = at(self.body, off);
        let seen = self.visited.iter().find(|v| v.0 == off).map(|v| v.1);
        let reread = kind == IfdKind::Gps && seen == Some(IfdKind::Interop);
        if off < 8 || dir.len() < 2 || (seen.is_some() && !reread) {
            return Ok(());
        }
        if depth > self.limits.max_depth {
            return Err(Error::LimitExceeded(Limit::Depth));
        }
        if self.visited.len() >= self.limits.max_ifds {
            return Err(Error::LimitExceeded(Limit::Ifds));
        }
        self.visited.push((off, kind));
        self.budget.check_cancel()?;

        let stated = usize::from(self.rd.u16(dir));
        let n = stated.min(dir.len().saturating_sub(2) / ENTRY_SIZE);
        if n > self.limits.max_entries {
            return Err(Error::LimitExceeded(Limit::Entries));
        }
        self.total_entries = self.total_entries.saturating_add(n);
        if self.total_entries > self.limits.max_total_entries {
            return Err(Error::LimitExceeded(Limit::TotalEntries));
        }
        let mut entries = self.budget.vec::<Entry>(n)?;
        // The sub-IFD pointers in entry order (the first pointer of each kind).
        let mut subs: [Option<(usize, IfdKind)>; 2] = [None; 2];
        let (mut thumb_off, mut thumb_len) = (None, None);

        for e in at(dir, 2).as_chunks::<ENTRY_SIZE>().0.iter().take(n) {
            let tag = self.rd.u16(e);
            let typ = self.rd.u16(at(e, 2));
            let count = to_usize(self.rd.u32(at(e, 4)));
            let Some(size) = type_size(typ).and_then(|ts| ts.checked_mul(count)) else {
                continue;
            };
            let data = if size <= 4 {
                e.get(8..8 + size)
            } else {
                let o = to_usize(self.rd.u32(at(e, 8)));
                o.checked_add(size).and_then(|end| self.body.get(o..end))
            };
            let Some(data) = data else { continue };
            self.add_value_bytes(size)?;
            let Some(value) = decode(typ, data, count, self.rd, self.budget)? else {
                continue;
            };
            let sub = match (kind, tag) {
                (IfdKind::Ifd0, TAG_EXIF_IFD) => Some(IfdKind::Exif),
                (IfdKind::Ifd0, TAG_GPS_IFD) => Some(IfdKind::Gps),
                (IfdKind::Exif, TAG_INTEROP_IFD) => Some(IfdKind::Interop),
                _ => None,
            };
            if let (Some(k), Value::Long(v)) = (sub, &value)
                && let Some(&p) = v.first()
                && !subs.iter().flatten().any(|s| s.1 == k)
                && let Some(slot) = subs.iter_mut().find(|s| s.is_none())
            {
                *slot = Some((to_usize(p), k));
            }
            match (kind, tag) {
                (IfdKind::Ifd1, TAG_THUMB_OFFSET) => thumb_off = thumb_off.or(first_uint(&value)),
                (IfdKind::Ifd1, TAG_THUMB_LENGTH) => thumb_len = thumb_len.or(first_uint(&value)),
                _ => {}
            }
            entries.push(Entry { tag, value });
        }
        if let Some(slot) = self.ifds.get_mut(kind.index()) {
            *slot = entries;
        }

        if kind == IfdKind::Ifd1
            && let (Some(o), Some(l)) = (thumb_off, thumb_len)
        {
            let (o, l) = (to_usize(o), to_usize(l));
            let bytes = o
                .checked_add(l)
                .and_then(|end| self.body.get(o..end).or_else(|| self.file.get(o..end)));
            if let Some(bytes) = bytes.filter(|b| !b.is_empty()) {
                self.add_value_bytes(bytes.len())?;
                self.thumbnail = Some(self.budget.copy(bytes)?);
            }
        }

        for (p, k) in subs.into_iter().flatten() {
            self.ifd(p, k, depth + 1)?;
        }
        if kind == IfdKind::Ifd0 {
            let np = 2 + stated * ENTRY_SIZE;
            if let Some(b) = dir.get(np..np + 4) {
                let next = self.rd.u32(b);
                if next != 0 {
                    self.ifd(to_usize(next), IfdKind::Ifd1, 0)?;
                }
            }
        }
        Ok(())
    }
}

/// Parses a TIFF block (with or without a leading `Exif\0\0`).
/// `file` is the file from the TIFF header on (empty when unknown): ExifTool reads a thumbnail
/// that ends past the block from the file.
pub(crate) fn parse(
    tiff: &[u8],
    file: &[u8],
    limits: &Limits,
    budget: &Budget<'_>,
) -> Result<Exif, Error> {
    if tiff.len() > limits.max_exif_bytes {
        return Err(Error::LimitExceeded(Limit::ExifBytes));
    }
    let (_, body, order) = header(tiff)?;
    let rd = Rd(order);
    let ifd0 = to_usize(rd.u32(at(body, 4)));
    if ifd0 < 8 || at(body, ifd0).len() < 2 {
        return Err(Error::Malformed("ifd0 offset"));
    }
    let mut walk = Walk {
        body,
        file,
        rd,
        limits,
        budget,
        ifds: Default::default(),
        visited: Vec::new(),
        total_entries: 0,
        value_bytes: 0,
        thumbnail: None,
    };
    walk.ifd(ifd0, IfdKind::Ifd0, 0)?;
    Ok(Exif {
        order,
        ifds: walk.ifds,
        thumbnail: walk.thumbnail,
    })
}

/// The byte range in `tiff` of the first value of IFD0 Orientation (0x0112, the first entry
/// the walk keeps, inline or out of line), and the byte order; `None` when that entry is not of
/// an integer type (BYTE, SHORT, LONG, SBYTE, SSHORT, SLONG or IFD) or has no value. The range
/// counts a leading `Exif\0\0` when `tiff` has one.
pub(crate) fn orientation_value_range(tiff: &[u8]) -> Option<(Range<usize>, ByteOrder)> {
    let (skip, body, order) = header(tiff).ok()?;
    let rd = Rd(order);
    let ifd = to_usize(rd.u32(at(body, 4)));
    let dir = body.get(ifd..).filter(|d| ifd >= 8 && d.len() >= 2)?;
    let stated = usize::from(rd.u16(dir));
    for (k, e) in at(dir, 2)
        .as_chunks::<ENTRY_SIZE>()
        .0
        .iter()
        .take(stated)
        .enumerate()
    {
        if rd.u16(e) != TAG_ORIENTATION {
            continue;
        }
        // The walk skips an entry of an unknown type or whose value does not fit in the block;
        // so does this. The first entry it keeps is the one ExifTool reports.
        let typ = rd.u16(at(e, 2));
        let count = to_usize(rd.u32(at(e, 4)));
        let Some(size) = type_size(typ).and_then(|ts| ts.checked_mul(count)) else {
            continue;
        };
        let start = if size <= 4 {
            ifd + 2 + k * ENTRY_SIZE + 8
        } else {
            to_usize(rd.u32(at(e, 8)))
        };
        if start.checked_add(size).is_none_or(|end| end > body.len()) {
            continue;
        }
        let width: usize = match typ {
            1 | 6 => 1,
            3 | 8 => 2,
            4 | 9 | 13 => 4,
            _ => return None,
        };
        if size == 0 {
            return None;
        }
        // An out-of-line value that overlaps the TIFF header or the IFD0 directory (a hostile
        // offset) is not written: the write would change the directory itself, so a second reset
        // would change the file again (found by the strip fuzz target).
        let dir_end = ifd.saturating_add(2 + stated.saturating_mul(ENTRY_SIZE) + 4);
        if size > 4 && (start < 8 || (start < dir_end && start + width > ifd)) {
            return None;
        }
        return Some((skip + start..skip + start + width, order));
    }
    None
}
