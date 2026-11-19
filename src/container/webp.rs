//! WebP: the RIFF chunks, and the rewrite of them.

use super::{
    Container, Raw, Walk, Want, capped_copy, exif_body, le16, le32, out_vec, splice_one, xmp_span,
};
use crate::budget::Budget;
use crate::rewrite::{kept_exif, reset_exif_in_place};
use crate::{Error, Keep, Limit, Limits};

/// VP8X flags: ICC profile, alpha, EXIF, XMP.
const FLAG_ICC: u8 = 0x20;
const FLAG_EXIF: u8 = 0x08;
const FLAG_XMP: u8 = 0x04;
const FLAG_ALPHA: u8 = 0x10;

/// One chunk: `start..end` is the chunk with its header and its pad byte (when it is there),
/// `dstart..dend` its data.
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
    /// The chunks end at the RIFF size.
    Done,
    /// The data ends inside a chunk.
    Trunc,
}

struct Chunks<'a, 'b> {
    data: &'a [u8],
    pos: usize,
    limit: usize,
    walk: Walk<'a, 'b>,
}

impl<'a, 'b> Chunks<'a, 'b> {
    fn new(data: &'a [u8], limits: &'a Limits, budget: &'a Budget<'b>) -> Self {
        // The RIFF size is not trusted: the chunks end at the smaller of it and the data.
        let riff_end = le32(data, 4).map_or(data.len(), |s| (s as usize).saturating_add(8));
        Self {
            data,
            pos: 12,
            limit: data.len().min(riff_end).max(12),
            walk: Walk::new(limits, budget),
        }
    }

    fn step(&mut self) -> Result<Item, Error> {
        let d = self.data;
        let start = self.pos;
        if start >= self.limit {
            return Ok(Item::Done);
        }
        self.walk.tick()?;
        let (Some(kind), Some(size)) = (
            d.get(start..start + 4)
                .and_then(|k| <[u8; 4]>::try_from(k).ok()),
            le32(d, start + 4),
        ) else {
            return Ok(Item::Trunc);
        };
        let dstart = start + 8;
        let dend = dstart.saturating_add(size as usize);
        if dend > self.limit {
            return Ok(Item::Trunc);
        }
        let end = if size & 1 == 1 && dend < self.limit {
            dend + 1
        } else {
            dend
        };
        self.pos = end;
        Ok(Item::Chunk(Chunk {
            kind,
            start,
            dstart,
            dend,
            end,
        }))
    }
}

pub(crate) fn extract(
    data: &[u8],
    limits: &Limits,
    budget: &Budget,
    want: Want,
) -> Result<Raw, Error> {
    let mut raw = Raw {
        container: Container::Webp,
        exif: None,
        xmp: None,
        xmp_extended: None,
        icc: None,
    };
    let mut chunks = Chunks::new(data, limits, budget);
    while let Item::Chunk(c) = chunks.step()? {
        let d = c.data(data);
        match &c.kind {
            b"EXIF" if want.exif && raw.exif.is_none() => {
                raw.exif = Some(capped_copy(
                    budget,
                    exif_body(d),
                    limits.max_exif_bytes,
                    Limit::ExifBytes,
                )?);
            }
            // "XMP\0": written by some software; ExifTool reads it too.
            b"XMP " | b"XMP\0" if want.xmp && raw.xmp.is_none() => {
                raw.xmp = Some(capped_copy(
                    budget,
                    d,
                    limits.max_xmp_bytes,
                    Limit::XmpBytes,
                )?);
            }
            b"ICCP" if want.icc && raw.icc.is_none() => {
                raw.icc = Some(budget.copy(d)?);
            }
            _ => {}
        }
    }
    Ok(raw)
}

/// Writes a chunk header, `d` and the pad byte.
fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], d: &[u8]) {
    out.extend_from_slice(kind);
    out.extend_from_slice(&u32::try_from(d.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(d);
    if d.len() & 1 == 1 {
        out.push(0);
    }
}

/// A VP8X chunk for a simple file, from the size in the VP8 or VP8L bitstream header.
fn vp8x_for(kind: &[u8; 4], d: &[u8]) -> Option<[u8; 18]> {
    let (w, h, alpha) = if kind == b"VP8L" {
        if *d.first()? != 0x2F {
            return None;
        }
        let v = le32(d, 1)?;
        (
            (v & 0x3FFF) + 1,
            ((v >> 14) & 0x3FFF) + 1,
            (v >> 28) & 1 == 1,
        )
    } else {
        if !d
            .get(3..)
            .is_some_and(|s| s.starts_with(&[0x9D, 0x01, 0x2A]))
        {
            return None;
        }
        (
            u32::from(le16(d, 6)? & 0x3FFF),
            u32::from(le16(d, 8)? & 0x3FFF),
            false,
        )
    };
    if w == 0 || h == 0 {
        return None;
    }
    let (wm, hm) = ((w - 1).to_le_bytes(), (h - 1).to_le_bytes());
    let flags = if alpha { FLAG_ALPHA } else { 0 };
    Some([
        b'V', b'P', b'8', b'X', 10, 0, 0, 0, flags, 0, 0, 0, wm[0], wm[1], wm[2], hm[0], hm[1],
        hm[2],
    ])
}

pub(crate) fn strip(
    data: &[u8],
    keep: Keep,
    limits: &Limits,
    budget: &Budget,
) -> Result<Vec<u8>, Error> {
    let mut exif = if keep.orientation {
        kept_exif(
            extract(data, limits, budget, Want::EXIF)?.exif,
            limits,
            budget,
        )?
    } else {
        None
    };
    let mut out = out_vec(budget, data.len())?;
    out.extend_from_slice(b"RIFF\0\0\0\0WEBP");
    let mut flags_at: Option<usize> = None;
    let mut icc_kept = false;
    let mut chunks = Chunks::new(data, limits, budget);
    loop {
        let c = match chunks.step()? {
            Item::Chunk(c) => c,
            Item::Done => break,
            Item::Trunc => return Err(Error::Truncated),
        };
        let d = c.data(data);
        match &c.kind {
            b"VP8X" if flags_at.is_none() && d.len() >= 10 => {
                flags_at = Some(out.len() + 8);
                push_chunk(&mut out, &c.kind, d);
            }
            b"VP8 " | b"VP8L" => {
                if flags_at.is_none() && exif.is_some() {
                    if let Some(x) = vp8x_for(&c.kind, d) {
                        flags_at = Some(out.len() + 8);
                        out.extend_from_slice(&x);
                    } else {
                        exif = None;
                    }
                }
                push_chunk(&mut out, &c.kind, d);
            }
            b"ALPH" | b"ANIM" | b"ANMF" => push_chunk(&mut out, &c.kind, d),
            b"ICCP" if keep.icc => {
                icc_kept = true;
                push_chunk(&mut out, &c.kind, d);
            }
            _ => {}
        }
    }
    let mut exif_added = false;
    if let (Some(t), Some(_)) = (exif, flags_at) {
        push_chunk(&mut out, b"EXIF", &t);
        exif_added = true;
    }
    if let Some(f) = flags_at.and_then(|at| out.get_mut(at)) {
        *f &= !(FLAG_ICC | FLAG_EXIF | FLAG_XMP);
        if icc_kept {
            *f |= FLAG_ICC;
        }
        if exif_added {
            *f |= FLAG_EXIF;
        }
    }
    let riff = u32::try_from(out.len().saturating_sub(8))
        .map_err(|_| Error::Malformed("webp riff size"))?;
    if let Some(d) = out.get_mut(4..8) {
        d.copy_from_slice(&riff.to_le_bytes());
    }
    Ok(out)
}

pub(crate) fn reset(data: &[u8], limits: &Limits, budget: &Budget) -> Result<Vec<u8>, Error> {
    let mut exif: Option<Chunk> = None;
    let mut xmp: Option<(Chunk, std::ops::Range<usize>)> = None;
    let mut xmp_seen = false;
    let mut chunks = Chunks::new(data, limits, budget);
    while let Item::Chunk(c) = chunks.step()? {
        if c.kind == *b"EXIF" && exif.is_none() {
            exif = Some(c);
        } else if (c.kind == *b"XMP " || c.kind == *b"XMP\0") && !xmp_seen {
            xmp_seen = true;
            if let Some(r) = xmp_span(c.data(data), limits) {
                xmp = Some((c, c.dstart + r.start..c.dstart + r.end));
            }
        }
    }
    let mut out = out_vec(budget, data.len())?;
    out.extend_from_slice(data);
    if let Some(c) = exif
        && let Some(block) = out.get_mut(c.dstart..c.dend)
    {
        reset_exif_in_place(block);
    }
    if let Some((c, span)) = xmp {
        let old_size = c.dend - c.dstart;
        let new_size = old_size - span.len() + 1;
        let old_pad = c.end - c.dend;
        let new_pad = new_size & 1;
        let size = u32::try_from(new_size).map_err(|_| Error::Malformed("webp chunk size"))?;
        splice_one(&mut out, span);
        if let Some(d) = out.get_mut(c.start + 4..c.start + 8) {
            d.copy_from_slice(&size.to_le_bytes());
        }
        let pad_at = c.dstart + new_size;
        if old_pad == 1 && new_pad == 0 && pad_at < out.len() {
            out.remove(pad_at);
        } else if old_pad == 0 && new_pad == 1 && pad_at <= out.len() {
            out.insert(pad_at, 0);
        }
        // The RIFF size follows the change of this chunk.
        let (old_total, new_total) = (old_size + old_pad, new_size + new_pad);
        if let Some(riff) = le32(&out, 4) {
            let diff = u32::try_from(old_total.abs_diff(new_total)).unwrap_or(u32::MAX);
            let adjusted = if new_total >= old_total {
                riff.checked_add(diff)
            } else {
                riff.checked_sub(diff)
            };
            let riff = adjusted.ok_or(Error::Malformed("webp riff size"))?;
            if let Some(d) = out.get_mut(4..8) {
                d.copy_from_slice(&riff.to_le_bytes());
            }
        }
    }
    Ok(out)
}
