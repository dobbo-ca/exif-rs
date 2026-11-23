//! JPEG: the segments from SOI to the first scan, and the rewrite of that header.

use std::ops::Range;

use super::mpf::{self, MPF_SIG, OffsetMap};
use super::{
    Container, EXIF_PREFIX, Raw, Walk, Want, XMP_SIG, be16, be32, capped_copy, out_vec, splice_one,
    xmp_span, zeroed,
};
use crate::budget::Budget;
use crate::rewrite::{MIN_EXIF_LEN, kept_exif, reset_exif_in_place};
use crate::xmp::{self, ContainerDesc, ITEM_LENGTH, ITEM_PADDING};
use crate::{Error, Keep, Limit, Limits};

const EXT_SIG: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";
const ICC_SIG: &[u8] = b"ICC_PROFILE\0";
const PS_SIG: &[u8] = b"Photoshop 3.0\0";
const PS_OLD_SIG: &[u8] = b"Adobe_Photoshop2.5:";
/// The longest XMP packet an APP1 segment holds after its length field and signature.
const MAX_XMP_PACKET: usize = 0xFFFF - 2 - XMP_SIG.len();

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
    /// `FF D9`; the offset after it.
    Eoi(usize),
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
            0xD9 => Ok(Item::Eoi(q)),
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
/// Writes the new segments that go before the first kept segment: the minimal EXIF, then the
/// new XMP packet (whose charge is given back once it is copied).
fn put_new(
    out: &mut Vec<u8>,
    exif: &mut Option<[u8; MIN_EXIF_LEN]>,
    xmp: &mut Option<Vec<u8>>,
    budget: &Budget,
) {
    if let Some(t) = exif.take() {
        out.extend_from_slice(&[0xFF, 0xE1, 0x00, MIN_APP1_LEN]);
        out.extend_from_slice(EXIF_PREFIX);
        out.extend_from_slice(&t);
    }
    if let Some(p) = xmp.take() {
        // `tail_of` keeps the packet within MAX_XMP_PACKET, so the length fits.
        let len = u16::try_from(2 + XMP_SIG.len() + p.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(XMP_SIG);
        out.extend_from_slice(&p);
        budget.release(p.len());
    }
}

/// The TIFF block of an APP1 payload ExifTool reads as EXIF: `Exif\0` in any case after at most
/// 4 bytes, and a byte after it; the block starts after that byte.
fn exif_app1_tiff(p: &[u8]) -> Option<&[u8]> {
    (0..=4).find_map(|i| {
        p.get(i..i + 5)
            .filter(|w| w.eq_ignore_ascii_case(b"exif\0"))
            .and_then(|_| p.get(i + 6..))
    })
}

/// The minimal EXIF block for `keep.orientation`: from the first APP1 before the first scan
/// that `strip` drops as EXIF ([`exif_app1_tiff`]) whose block has a valid IFD0 Orientation. The
/// block [`extract`] takes (the first APP1 `Exif\0\0`) is read as it gives it, joined with its
/// continuation segments; any other is the TIFF block of its segment.
fn kept_orientation(
    data: &[u8],
    limits: &Limits,
    budget: &Budget,
) -> Result<Option<[u8; MIN_EXIF_LEN]>, Error> {
    let mut extract_seen = false;
    let mut segs = Segments::new(data, limits, budget);
    while let Item::Seg(s) = segs.step()? {
        let p = s.payload(data);
        let Some(t) = exif_app1_tiff(p).filter(|_| s.marker == 0xE1) else {
            continue;
        };
        let block = if !extract_seen && p.starts_with(EXIF_PREFIX) {
            extract_seen = true;
            extract(data, limits, budget, Want::EXIF)?.exif
        } else {
            Some(capped_copy(
                budget,
                t,
                limits.max_exif_bytes,
                Limit::ExifBytes,
            )?)
        };
        if let Some(k) = kept_exif(block, limits, budget)? {
            return Ok(Some(k));
        }
    }
    Ok(None)
}

/// An APP1 payload ExifTool reads as XMP: standard or extended XMP, any other `http` header,
/// `XMP\0`, or one that holds `<exif:` or `<?xpacket`.
fn is_xmp_app1(p: &[u8]) -> bool {
    p.starts_with(b"http")
        || p.starts_with(b"XMP\0")
        || p.windows(6).any(|w| w == b"<exif:")
        || p.windows(9).any(|w| w == b"<?xpacket")
}

fn keep_segment(marker: u8, p: &[u8], keep: Keep) -> bool {
    match marker {
        0xE0 if p.starts_with(b"JFIF\0") => true,
        0xEE if p.starts_with(b"Adobe") => true,
        0xE1 if exif_app1_tiff(p).is_some() || is_xmp_app1(p) => false,
        0xE2 if p.starts_with(ICC_SIG) => keep.icc,
        0xE2 if p.starts_with(MPF_SIG) => keep.mpf_images && keep.trailing,
        0xED if p.starts_with(PS_SIG) || p.starts_with(PS_OLD_SIG) => false,
        0xE0..=0xEF => keep.unknown,
        0xFE => false,
        _ => true,
    }
}

/// What a strip with `keep.trailing` does after the primary image.
struct Tail {
    /// The offset after the EOI of the primary image (the data length when it has none).
    end: usize,
    /// The MPF images that are dropped, in file order.
    dropped: Vec<Range<usize>>,
    /// The new XMP packet that describes the bytes kept after the EOI.
    xmp: Option<Vec<u8>>,
}

fn tail_of(data: &[u8], keep: Keep, limits: &Limits, budget: &Budget) -> Result<Tail, Error> {
    let mut mpf_seg: Option<Seg> = None;
    let mut packet: Option<&[u8]> = None;
    let mut segs = Segments::new(data, limits, budget);
    let end = loop {
        match segs.step()? {
            Item::Seg(s) => {
                let p = s.payload(data);
                if s.marker == 0xE2 && mpf_seg.is_none() && p.starts_with(MPF_SIG) {
                    mpf_seg = Some(s);
                }
                if s.marker == 0xE1
                    && packet.is_none()
                    && let Some(b) = p.strip_prefix(XMP_SIG)
                {
                    packet = Some(b);
                }
            }
            Item::Scan(off) => break segs.scan_end(off)?,
            Item::Eoi(end) => break end,
            // The strip fails on these.
            Item::Trunc | Item::Bad => break data.len(),
        }
    };

    let mut dropped = Vec::new();
    if !keep.mpf_images
        && let Some(s) = mpf_seg
    {
        let in_tiff = s.pstart + MPF_SIG.len();
        let t = data.get(in_tiff..s.end).unwrap_or_default();
        dropped = budget.vec(mpf::images(t).count())?;
        for (off, size) in mpf::images(t) {
            let Some(r) = in_tiff
                .checked_add(off)
                .and_then(|a| a.checked_add(size).map(|e| a..e))
            else {
                continue;
            };
            if size >= 2
                && r.start >= end
                && r.end <= data.len()
                && data.get(r.start..r.start + 2) == Some(&[0xFFu8, 0xD8][..])
            {
                dropped.push(r);
            }
        }
        // An image that overlaps another one is not dropped.
        dropped.sort_unstable_by_key(|r| r.start);
        let mut max_end = 0;
        for i in 0..dropped.len() {
            let next = dropped.get(i + 1).map(|n| n.start);
            let Some(r) = dropped.get_mut(i) else {
                continue;
            };
            let alone = r.start >= max_end && next.is_none_or(|n| n >= r.end);
            max_end = max_end.max(r.end);
            if !alone {
                r.end = r.start;
            }
        }
        dropped.retain(|r| !r.is_empty());
    }

    let gone = dropped
        .iter()
        .fold(0usize, |n, r| n.saturating_add(r.len()));
    let kept = data.len().saturating_sub(end).saturating_sub(gone);
    let mut xmp_out = None;
    if kept > 0
        && let Some(p) = packet
    {
        let mut d = xmp::container_desc(p, limits, budget)?;
        if !dropped.is_empty() {
            d.version = None;
            if !drop_items(&mut d, p, &dropped, end, data.len()) {
                d.items.clear();
            }
        }
        xmp_out = xmp::container_packet(p, &d, MAX_XMP_PACKET)?;
    }
    Ok(Tail {
        end,
        dropped,
        xmp: xmp_out,
    })
}

/// The number in the field `i` of a directory item; `default` when it has none.
fn item_number(packet: &[u8], it: &xmp::Item, i: usize, default: Option<usize>) -> Option<usize> {
    match it.get(i).and_then(Option::as_ref) {
        Some(r) => xmp::number(packet, r),
        None => default,
    }
}

/// Removes the directory items that are the dropped MPF images (`dropped`, in file order). The
/// items after the first (the primary image) are located as readers do, from the end of the file:
/// each is `Item:Length` bytes and is followed by its `Item:Padding` bytes, the last one ending at
/// the end of the file, and the padding of the first item leads from the primary image's EOI to
/// the second item. `false` when that layout does not hold, an item after the first has no length,
/// a dropped image is not exactly one item, or a dropped item has padding.
fn drop_items(
    d: &mut ContainerDesc,
    packet: &[u8],
    dropped: &[Range<usize>],
    primary_end: usize,
    file_len: usize,
) -> bool {
    let len_pad = |it: &xmp::Item| {
        Some((
            item_number(packet, it, ITEM_LENGTH, None)?,
            item_number(packet, it, ITEM_PADDING, Some(0))?,
        ))
    };
    let mut total = 0usize;
    for it in d.items.iter().skip(1) {
        let Some(t) = len_pad(it).and_then(|(l, p)| total.checked_add(l)?.checked_add(p)) else {
            return false;
        };
        total = t;
    }
    let pad0 = match d.items.first() {
        Some(it) => item_number(packet, it, ITEM_PADDING, Some(0)),
        None => Some(0),
    };
    let Some(start) = pad0.and_then(|p| primary_end.checked_add(p)) else {
        return false;
    };
    if file_len.checked_sub(total) != Some(start) {
        return false;
    }
    let mut at = start;
    let mut found = 0usize;
    let mut ok = true;
    let mut first = true;
    d.items.retain(|it| {
        if std::mem::take(&mut first) {
            return true;
        }
        // Every item after the first has a length and they add up to the file: no overflow.
        let (len, pad) = len_pad(it).unwrap_or((0, 0));
        let r = at..at.saturating_add(len);
        at = r.end.saturating_add(pad);
        let gone = dropped
            .binary_search_by_key(&r.start, |g| g.start)
            .ok()
            .and_then(|i| dropped.get(i))
            == Some(&r);
        if gone {
            found += 1;
            ok &= pad == 0;
        }
        !gone
    });
    ok && found == dropped.len()
}

/// Copies the bytes `r` of `data` to `out`, and records where they went.
fn copy(
    out: &mut Vec<u8>,
    data: &[u8],
    r: Range<usize>,
    map: Option<&mut OffsetMap>,
) -> Result<(), Error> {
    if let Some(b) = data.get(r.clone()) {
        let at = out.len();
        out.extend_from_slice(b);
        if let Some(m) = map {
            m.push(r, at)?;
        }
    }
    Ok(())
}

/// Copies the rest of the primary image (`body`: its first scan to its EOI) and, with `tail`, the
/// kept bytes after it; then fixes the kept MPF segment (`mpf_at`: its TIFF header in the input
/// and in the output, and the end of its output copy).
fn finish(
    data: &[u8],
    body: Range<usize>,
    mut out: Vec<u8>,
    tail: Option<&Tail>,
    mut map: Option<OffsetMap>,
    mpf_at: Option<(usize, usize, usize)>,
) -> Result<Vec<u8>, Error> {
    copy(&mut out, data, body, map.as_mut())?;
    let primary_len = out.len();
    if let Some(t) = tail {
        let mut at = t.end;
        for r in &t.dropped {
            copy(&mut out, data, at..r.start, map.as_mut())?;
            at = r.end;
        }
        copy(&mut out, data, at..data.len(), map.as_mut())?;
    }
    if let (Some(m), Some((in_tiff, out_tiff, out_end))) = (&map, mpf_at)
        && let Some(t) = out.get_mut(out_tiff..out_end)
    {
        mpf::patch(t, in_tiff, out_tiff, primary_len, m);
    }
    Ok(out)
}

pub(crate) fn strip(
    data: &[u8],
    keep: Keep,
    limits: &Limits,
    budget: &Budget,
) -> Result<Vec<u8>, Error> {
    let mut pending = if keep.orientation {
        kept_orientation(data, limits, budget)?
    } else {
        None
    };
    let mut tail = if keep.trailing {
        Some(tail_of(data, keep, limits, budget)?)
    } else {
        None
    };
    let mut pending_xmp = tail.as_mut().and_then(|t| t.xmp.take());
    let xmp_len = pending_xmp
        .as_ref()
        .map_or(0, |p| 4 + XMP_SIG.len() + p.len());
    let mut out = out_vec(budget, data.len().saturating_add(xmp_len))?;
    out.extend_from_slice(&[0xFF, 0xD8]);
    // Where the kept input bytes went, for the MPF fix-up.
    let mut map = (keep.mpf_images && keep.trailing).then(|| OffsetMap::new(budget));
    let mut mpf_at = None;
    let mut segs = Segments::new(data, limits, budget);
    let mut first = true;
    loop {
        match segs.step()? {
            Item::Seg(s) => {
                let p = s.payload(data);
                let first_jfif = first && s.marker == 0xE0 && p.starts_with(b"JFIF\0");
                first = false;
                if !first_jfif {
                    put_new(&mut out, &mut pending, &mut pending_xmp, budget);
                }
                if keep_segment(s.marker, p, keep) {
                    let at = out.len();
                    copy(&mut out, data, s.start..s.end, map.as_mut())?;
                    if s.marker == 0xE2 && mpf_at.is_none() && p.starts_with(MPF_SIG) {
                        let tiff = s.pstart + MPF_SIG.len();
                        mpf_at = Some((tiff, at + (tiff - s.start), at + (s.end - s.start)));
                    }
                }
                if first_jfif {
                    put_new(&mut out, &mut pending, &mut pending_xmp, budget);
                }
            }
            Item::Scan(off) => {
                put_new(&mut out, &mut pending, &mut pending_xmp, budget);
                let end = match &tail {
                    Some(t) => t.end,
                    None => segs.scan_end(off)?,
                };
                return finish(data, off..end, out, tail.as_ref(), map, mpf_at);
            }
            Item::Eoi(end) => {
                put_new(&mut out, &mut pending, &mut pending_xmp, budget);
                let eoi = end.saturating_sub(2)..end;
                return finish(data, eoi, out, tail.as_ref(), map, mpf_at);
            }
            Item::Trunc => return Err(Error::Truncated),
            Item::Bad => return Err(Error::Malformed("jpeg marker")),
        }
    }
}

pub(crate) fn reset(data: &[u8], limits: &Limits, budget: &Budget) -> Result<Vec<u8>, Error> {
    // The orientation value of the EXIF block as `extract` joins it (with its continuation
    // segments), set to 1: its range in the block and its new bytes.
    let mark = budget.used();
    let fix = extract(data, limits, budget, Want::EXIF)?
        .exif
        .and_then(|mut block| {
            let (r, _) = crate::tiff::orientation_value_range(&block)?;
            reset_exif_in_place(&mut block);
            let mut v = [0u8; 4];
            let src = block.get(r.clone())?;
            v.get_mut(..src.len())?.copy_from_slice(src);
            Some((r, v))
        });
    budget.reset_to(mark);
    // The file offset of each byte of the value (at most 4) and its new value.
    let mut put = [None::<(usize, u8)>; 4];
    // The block offset where the next continuation segment goes, while the block is open.
    let mut exif_at: Option<usize> = None;
    let mut exif_seen = false;
    let mut xmp: Option<(Seg, Range<usize>)> = None;
    let mut xmp_seen = false;
    // The first MPF segment, fixed when the XMP value changes length.
    let mut mpf_seg: Option<Seg> = None;
    let mut segs = Segments::new(data, limits, budget);
    let last = loop {
        let s = match segs.step()? {
            Item::Seg(s) => s,
            other => break other,
        };
        let open = exif_at.take();
        if s.marker == 0xE2 && mpf_seg.is_none() && s.payload(data).starts_with(MPF_SIG) {
            mpf_seg = Some(s);
        }
        if s.marker != 0xE1 {
            continue;
        }
        let p = s.payload(data);
        if p.starts_with(EXIF_PREFIX) {
            let at = if exif_seen {
                open.filter(|_| exif_continuation(p).is_some())
            } else {
                exif_seen = true;
                Some(0)
            };
            if let Some(at) = at {
                let start = s.pstart + EXIF_PREFIX.len();
                let len = s.end - start;
                // The block bytes of the value that fall in this segment, mapped to the file.
                for ((i, &b), slot) in fix
                    .iter()
                    .flat_map(|(r, v)| r.clone().zip(v))
                    .zip(put.iter_mut())
                {
                    if let Some(o) = i.checked_sub(at).filter(|&o| o < len) {
                        *slot = Some((start + o, b));
                    }
                }
                exif_at = at.checked_add(len);
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
    };
    let mut out = out_vec(budget, data.len())?;
    out.extend_from_slice(data);
    for (at, b) in put.into_iter().flatten() {
        if let Some(d) = out.get_mut(at) {
            *d = b;
        }
    }
    if let Some((s, span)) = xmp {
        let seg_len = (s.end - s.pstart + 2 + 1)
            .checked_sub(span.len())
            .and_then(|n| u16::try_from(n).ok())
            .ok_or(Error::Malformed("jpeg segment length"))?;
        splice_one(&mut out, span.clone());
        if let Some(d) = out.get_mut(s.pstart - 2..s.pstart) {
            d.copy_from_slice(&seg_len.to_be_bytes());
        }
        if span.len() != 1
            && let Some(m) = mpf_seg
        {
            let primary_end = match last {
                Item::Scan(off) => segs.scan_end(off)?,
                Item::Eoi(end) => end,
                _ => data.len(),
            };
            fix_mpf(&mut out, data.len(), span, m, primary_end, budget)?;
        }
    }
    Ok(out)
}

/// Fixes the MPF segment `m` of `out` after the XMP value at `span` (in the primary image's
/// header) became one byte: the bytes after `span` moved by `1 - span.len()`.
fn fix_mpf(
    out: &mut [u8],
    data_len: usize,
    span: Range<usize>,
    m: Seg,
    primary_end: usize,
    budget: &Budget,
) -> Result<(), Error> {
    let mut map = OffsetMap::new(budget);
    map.push(0..span.start, 0)?;
    map.push(span.end..data_len, span.start + 1)?;
    let in_tiff = m.pstart + MPF_SIG.len();
    let primary_len = (primary_end + 1).checked_sub(span.len());
    if let (Some(out_tiff), Some(out_seg), Some(primary_len)) =
        (map.get(in_tiff), map.get(m.start), primary_len)
        && let Some(t) = out.get_mut(out_tiff..out_seg + (m.end - m.start))
    {
        mpf::patch(t, in_tiff, out_tiff, primary_len, &map);
    }
    Ok(())
}
