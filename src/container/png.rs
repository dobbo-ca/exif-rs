//! PNG: the chunks from the signature to IEND, and the rewrite of them.

use std::ops::Range;

use super::{
    Container, EXIF_PREFIX, PNG_SIG, Raw, Walk, Want, be32, capped_copy, exif_body, out_vec,
    splice_one, xmp_span,
};
use crate::budget::Budget;
use crate::crc32::crc32;
use crate::rewrite::{MIN_EXIF_LEN, kept_exif, reset_exif_in_place};
use crate::{Error, Keep, Limit, Limits};

const XMP_KEYWORD: &[u8] = b"XML:com.adobe.xmp";

/// One chunk: `start..end` is the whole chunk (length, type, data, CRC), `dstart..dend` its data.
#[derive(Clone, Copy)]
struct Chunk {
    kind: [u8; 4],
    start: usize,
    dstart: usize,
    dend: usize,
    end: usize,
}

impl Chunk {
    fn data<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        data.get(self.dstart..self.dend).unwrap_or_default()
    }
}

enum Item {
    Chunk(Chunk),
    /// IEND was the last chunk.
    Done,
    /// The data ends inside a chunk or before IEND.
    Trunc,
    /// A chunk length over 2^31-1.
    Bad,
}

struct Chunks<'a, 'b> {
    data: &'a [u8],
    pos: usize,
    done: bool,
    walk: Walk<'a, 'b>,
}

impl<'a, 'b> Chunks<'a, 'b> {
    fn new(data: &'a [u8], limits: &'a Limits, budget: &'a Budget<'b>) -> Self {
        Self {
            data,
            pos: PNG_SIG.len(),
            done: false,
            walk: Walk::new(limits, budget),
        }
    }

    fn step(&mut self) -> Result<Item, Error> {
        if self.done {
            return Ok(Item::Done);
        }
        self.walk.tick()?;
        let d = self.data;
        let start = self.pos;
        let Some(len) = be32(d, start) else {
            return Ok(Item::Trunc);
        };
        if len > 0x7FFF_FFFF {
            return Ok(Item::Bad);
        }
        let Some(kind) = d
            .get(start + 4..start + 8)
            .and_then(|k| <[u8; 4]>::try_from(k).ok())
        else {
            return Ok(Item::Trunc);
        };
        let dstart = start + 8;
        let Some(end) = dstart
            .checked_add(len as usize)
            .and_then(|e| e.checked_add(4))
        else {
            return Ok(Item::Trunc);
        };
        let dend = end - 4;
        if end > d.len() {
            return Ok(Item::Trunc);
        }
        self.pos = end;
        self.done = kind == *b"IEND";
        Ok(Item::Chunk(Chunk {
            kind,
            start,
            dstart,
            dend,
            end,
        }))
    }
}

/// `d` split at its first NUL: the bytes before it and the bytes after it.
fn split_nul(d: &[u8]) -> Option<(&[u8], &[u8])> {
    let i = d.iter().position(|&b| b == 0)?;
    Some((d.get(..i)?, d.get(i + 1..)?))
}

/// An iTXt chunk: the keyword, whether the text is compressed, and where the text starts.
struct ITxt<'a> {
    keyword: &'a [u8],
    compressed: bool,
    method: u8,
    text_start: usize,
}

fn itxt(d: &[u8]) -> Option<ITxt<'_>> {
    let (keyword, r) = split_nul(d)?;
    let [flag, method, r @ ..] = r else {
        return None;
    };
    let (_language, r) = split_nul(r)?;
    let (_translated, r) = split_nul(r)?;
    Some(ITxt {
        keyword,
        compressed: *flag == 1,
        method: *method,
        text_start: d.len() - r.len(),
    })
}

fn is_raw_profile(keyword: &[u8]) -> bool {
    keyword == b"Raw profile type exif" || keyword == b"Raw profile type APP1"
}

/// Inflates a zlib stream under the remaining budget and `cap`. `None` when the stream is
/// unreadable. The output is charged.
fn inflate(
    src: &[u8],
    cap: usize,
    cap_limit: Limit,
    budget: &Budget,
) -> Result<Option<Vec<u8>>, Error> {
    let rem = budget.remaining();
    let max = rem.min(cap);
    let lim = deflate::Limits {
        max_output: max,
        max_alloc: max,
    };
    match deflate::zlib::decode_limit(src, usize::MAX, &lim, budget.cancel()) {
        Ok(v) => {
            budget.charge(v.len())?;
            Ok(Some(v))
        }
        Err(deflate::Error::OutputLimit) => Err(Error::LimitExceeded(if cap <= rem {
            cap_limit
        } else {
            Limit::Alloc
        })),
        Err(deflate::Error::AllocLimit) => Err(Error::LimitExceeded(Limit::Alloc)),
        Err(deflate::Error::OutOfMemory) => Err(Error::OutOfMemory),
        Err(deflate::Error::Cancelled) => Err(Error::Cancelled),
        Err(_) => Ok(None),
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decodes ImageMagick raw profile text (`\n<name>\n<spaces><decimal length>\n<hex lines>`)
/// into an EXIF block without `Exif\0\0`. `None` when the text is not one.
fn raw_profile(text: &[u8], limits: &Limits, budget: &Budget) -> Result<Option<Vec<u8>>, Error> {
    let mut i = 0;
    while text.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    while text.get(i).is_some_and(|b| !b.is_ascii_whitespace()) {
        i += 1;
    }
    while text.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    let mut n = 0usize;
    let mut digits = false;
    while let Some(&b) = text.get(i) {
        if !b.is_ascii_digit() {
            break;
        }
        n = n.saturating_mul(10).saturating_add(usize::from(b - b'0'));
        digits = true;
        i += 1;
    }
    if !digits || n == 0 {
        return Ok(None);
    }
    if n > limits.max_exif_bytes {
        return Err(Error::LimitExceeded(Limit::ExifBytes));
    }
    let mut out = budget.vec::<u8>(n)?;
    let mut hi: Option<u8> = None;
    for &b in text.get(i..).unwrap_or_default() {
        if b.is_ascii_whitespace() {
            continue;
        }
        let Some(v) = hex_val(b) else { break };
        match hi.take() {
            None => hi = Some(v),
            Some(h) => {
                out.push((h << 4) | v);
                if out.len() == n {
                    break;
                }
            }
        }
    }
    let tiff = out.starts_with(b"II*\0") || out.starts_with(b"MM\0*");
    if out.len() != n || !(tiff || out.starts_with(EXIF_PREFIX)) {
        budget.release(n);
        return Ok(None);
    }
    if out.starts_with(EXIF_PREFIX) {
        out.drain(..EXIF_PREFIX.len()).for_each(drop);
    }
    Ok(Some(out))
}

/// The EXIF block of a raw profile chunk whose text is `body` (inflated when `compressed`).
fn profile_from(
    body: &[u8],
    compressed: bool,
    limits: &Limits,
    budget: &Budget,
) -> Result<Option<Vec<u8>>, Error> {
    if !compressed {
        return raw_profile(body, limits, budget);
    }
    let cap = limits.max_exif_bytes.saturating_mul(2).saturating_add(1024);
    let Some(text) = inflate(body, cap, Limit::ExifBytes, budget)? else {
        return Ok(None);
    };
    let r = raw_profile(&text, limits, budget);
    budget.release(text.len());
    r
}

pub(crate) fn extract(
    data: &[u8],
    limits: &Limits,
    budget: &Budget,
    want: Want,
) -> Result<Raw, Error> {
    let mut raw = Raw {
        container: Container::Png,
        exif: None,
        xmp: None,
        xmp_extended: None,
        icc: None,
    };
    let mut exif_chunk = false;
    let mut chunks = Chunks::new(data, limits, budget);
    while let Item::Chunk(c) = chunks.step()? {
        let d = c.data(data);
        match &c.kind {
            b"eXIf" if want.exif && !exif_chunk => {
                let v = capped_copy(
                    budget,
                    exif_body(d),
                    limits.max_exif_bytes,
                    Limit::ExifBytes,
                )?;
                if let Some(old) = raw.exif.replace(v) {
                    budget.release(old.len());
                }
                exif_chunk = true;
            }
            b"iCCP" if want.icc && raw.icc.is_none() => {
                let Some((_name, r)) = split_nul(d) else {
                    continue;
                };
                let [0, z @ ..] = r else { continue };
                raw.icc = inflate(z, usize::MAX, Limit::Alloc, budget)?;
            }
            b"iTXt" => {
                let Some(t) = itxt(d) else { continue };
                let body = d.get(t.text_start..).unwrap_or_default();
                if t.keyword == XMP_KEYWORD {
                    if !want.xmp || raw.xmp.is_some() {
                        continue;
                    }
                    raw.xmp = if !t.compressed {
                        Some(capped_copy(
                            budget,
                            body,
                            limits.max_xmp_bytes,
                            Limit::XmpBytes,
                        )?)
                    } else if t.method == 0 {
                        inflate(body, limits.max_xmp_bytes, Limit::XmpBytes, budget)?
                    } else {
                        None
                    };
                } else if is_raw_profile(t.keyword)
                    && want.exif
                    && raw.exif.is_none()
                    && (!t.compressed || t.method == 0)
                {
                    raw.exif = profile_from(body, t.compressed, limits, budget)?;
                }
            }
            b"tEXt" if want.exif && raw.exif.is_none() => {
                if let Some((keyword, body)) = split_nul(d)
                    && is_raw_profile(keyword)
                {
                    raw.exif = profile_from(body, false, limits, budget)?;
                }
            }
            b"zTXt" if want.exif && raw.exif.is_none() => {
                if let Some((keyword, [0, z @ ..])) = split_nul(d)
                    && is_raw_profile(keyword)
                {
                    raw.exif = profile_from(z, true, limits, budget)?;
                }
            }
            _ => {}
        }
    }
    Ok(raw)
}

fn put_exif(out: &mut Vec<u8>, pending: &mut Option<[u8; MIN_EXIF_LEN]>) {
    if let Some(t) = pending.take() {
        out.extend_from_slice(&[0, 0, 0, 26]);
        out.extend_from_slice(b"eXIf");
        out.extend_from_slice(&t);
        out.extend_from_slice(&crc32(&[&b"eXIf"[..], &t[..]]).to_be_bytes());
    }
}

fn keep_chunk(kind: &[u8; 4], keep: Keep) -> bool {
    match kind {
        b"IHDR" | b"PLTE" | b"IDAT" | b"IEND" | b"tRNS" | b"cHRM" | b"gAMA" | b"sBIT" | b"sRGB"
        | b"cICP" | b"mDCV" | b"cLLI" | b"bKGD" | b"hIST" | b"pHYs" | b"sPLT" | b"acTL"
        | b"fcTL" | b"fdAT" => true,
        b"iCCP" => keep.icc,
        _ => false,
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
    out.extend_from_slice(&PNG_SIG);
    let mut chunks = Chunks::new(data, limits, budget);
    loop {
        match chunks.step()? {
            Item::Chunk(c) => {
                if c.kind == *b"IDAT" || c.kind == *b"IEND" {
                    put_exif(&mut out, &mut pending);
                }
                if keep_chunk(&c.kind, keep) {
                    out.extend_from_slice(data.get(c.start..c.end).unwrap_or_default());
                }
            }
            Item::Done => return Ok(out),
            Item::Trunc => return Err(Error::Truncated),
            Item::Bad => return Err(Error::Malformed("png chunk length")),
        }
    }
}

/// Writes the CRC of the chunk at `start` whose data ends at `dend` (type and data are
/// checksummed; the CRC field follows the data).
fn fix_crc(out: &mut [u8], start: usize, dend: usize) {
    let Some(crc) = out.get(start + 4..dend).map(|b| crc32(&[b])) else {
        return;
    };
    if let Some(d) = out.get_mut(dend..dend + 4) {
        d.copy_from_slice(&crc.to_be_bytes());
    }
}

pub(crate) fn reset(data: &[u8], limits: &Limits, budget: &Budget) -> Result<Vec<u8>, Error> {
    let mut exif: Option<Chunk> = None;
    let mut xmp: Option<(Chunk, Range<usize>)> = None;
    let mut xmp_seen = false;
    let mut chunks = Chunks::new(data, limits, budget);
    while let Item::Chunk(c) = chunks.step()? {
        if c.kind == *b"eXIf" && exif.is_none() {
            exif = Some(c);
        } else if c.kind == *b"iTXt"
            && !xmp_seen
            && let Some(t) = itxt(c.data(data))
            && t.keyword == XMP_KEYWORD
        {
            xmp_seen = true;
            if !t.compressed {
                let base = c.dstart + t.text_start;
                let text = data.get(base..c.dend).unwrap_or_default();
                if let Some(r) = xmp_span(text, limits) {
                    xmp = Some((c, base + r.start..base + r.end));
                }
            }
        }
    }
    let mut out = out_vec(budget, data.len())?;
    out.extend_from_slice(data);
    if let Some(c) = exif {
        if let Some(block) = out.get_mut(c.dstart..c.dend) {
            reset_exif_in_place(block);
        }
        fix_crc(&mut out, c.start, c.dend);
    }
    if let Some((c, span)) = xmp {
        let new_dend = c.dend - span.len() + 1;
        let new_len = u32::try_from(new_dend - c.dstart)
            .ok()
            .filter(|&n| n <= 0x7FFF_FFFF)
            .ok_or(Error::Malformed("png chunk length"))?;
        splice_one(&mut out, span);
        if let Some(d) = out.get_mut(c.start..c.start + 4) {
            d.copy_from_slice(&new_len.to_be_bytes());
        }
        fix_crc(&mut out, c.start, new_dend);
    }
    Ok(out)
}
