//! JPEG: the segments from SOI to the first scan, and the rewrite of that header.

use std::ops::Range;

use super::{
    Container, EXIF_PREFIX, Raw, Walk, Want, be16, be32, capped_copy, out_vec, splice_one,
    xmp_span, zeroed,
};
use crate::budget::Budget;
use crate::rewrite::{MIN_EXIF_LEN, kept_exif, reset_exif_in_place};
use crate::{Error, Keep, Limit, Limits};

const XMP_SIG: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
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

fn sum_icc(lens: &[Option<usize>; 256], count: u8) -> Option<usize> {
    lens.get(1..=usize::from(count))?
        .iter()
        .try_fold(0usize, |t, l| t.checked_add((*l)?))
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
    let mut icc_count = 0u8;
    let mut icc_len = [None::<usize>; 256];
    let mut segs = Segments::new(data, limits, budget);
    while let Item::Seg(s) = segs.step()? {
        let p = s.payload(data);
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
            0xE2 if want.icc => {
                if let Some([seq, count, rest @ ..]) = p.strip_prefix(ICC_SIG) {
                    if *seq == 0 || seq > count {
                        continue;
                    }
                    if icc_count == 0 {
                        icc_count = *count;
                    } else if icc_count != *count {
                        continue;
                    }
                    if let Some(slot) = icc_len.get_mut(usize::from(*seq))
                        && slot.is_none()
                    {
                        *slot = Some(rest.len());
                    }
                }
            }
            _ => {}
        }
    }

    let icc_total = if icc_count > 0 {
        sum_icc(&icc_len, icc_count).filter(|&t| t > 0)
    } else {
        None
    };
    let guid = if want.xmp {
        raw.xmp.as_deref().and_then(find_guid)
    } else {
        None
    };
    let mut icc = match icc_total {
        Some(t) => Some(zeroed(budget, t)?),
        None => None,
    };
    let mut ext: Option<Vec<u8>> = None;
    if guid.is_some() || icc.is_some() {
        let std_len = raw.xmp.as_ref().map_or(0, Vec::len);
        let mut seen = [false; 256];
        let mut segs = Segments::new(data, limits, budget);
        while let Item::Seg(s) = segs.step()? {
            let p = s.payload(data);
            match s.marker {
                0xE1 => {
                    let Some(g) = guid.as_ref() else { continue };
                    let Some(c) = p.strip_prefix(EXT_SIG).and_then(ext_chunk) else {
                        continue;
                    };
                    if !c.guid.eq_ignore_ascii_case(g)
                        || c.full == 0
                        || !c.off.checked_add(c.data.len()).is_some_and(|e| e <= c.full)
                    {
                        continue;
                    }
                    if ext.is_none() {
                        if c.full
                            .checked_add(std_len)
                            .is_none_or(|t| t > limits.max_xmp_bytes)
                        {
                            return Err(Error::LimitExceeded(Limit::XmpBytes));
                        }
                        ext = Some(zeroed(budget, c.full)?);
                    }
                    if let Some(buf) = ext.as_mut()
                        && buf.len() == c.full
                        && let Some(dst) = buf.get_mut(c.off..c.off + c.data.len())
                    {
                        dst.copy_from_slice(c.data);
                    }
                }
                0xE2 => {
                    let Some(buf) = icc.as_mut() else { continue };
                    let Some([seq, count, rest @ ..]) = p.strip_prefix(ICC_SIG) else {
                        continue;
                    };
                    let i = usize::from(*seq);
                    if i == 0 || *count != icc_count || seq > count {
                        continue;
                    }
                    if let Some(done) = seen.get_mut(i) {
                        if *done {
                            continue;
                        }
                        *done = true;
                    }
                    let off = icc_len
                        .get(1..i)
                        .map_or(0, |l| l.iter().flatten().sum::<usize>());
                    if let Some(dst) = buf.get_mut(off..off.saturating_add(rest.len())) {
                        dst.copy_from_slice(rest);
                    }
                }
                _ => {}
            }
        }
    }
    raw.icc = icc;
    raw.xmp_extended = ext;
    Ok(raw)
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
            if let Some(r) = xmp_span(b, limits) {
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
