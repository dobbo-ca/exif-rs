//! Strip and rewrite: new file bytes with the metadata removed, or the orientation reset.

use std::sync::atomic::AtomicBool;

use crate::budget::Budget;
use crate::container::{self, Container, jpeg, png, webp};
use crate::{ByteOrder, Error, Exif, Limits};

/// What [`strip`] keeps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Keep {
    /// Keep the EXIF IFD0 Orientation, in a new minimal EXIF block (nothing else of the EXIF).
    pub orientation: bool,
    /// Keep the ICC profile, as stored.
    pub icc: bool,
}

/// The length of the minimal EXIF block: TIFF header, IFD0 with one entry, next IFD 0.
pub(crate) const MIN_EXIF_LEN: usize = 26;

/// Removes all metadata from a JPEG, PNG or WebP file, except what `keep` names. The image
/// data bytes are never changed.
///
/// - JPEG: before the first scan keeps SOI, APP0 `JFIF` (as stored, with its thumbnail if it
///   has one), APP14 `Adobe`, APP2 ICC (with
///   `keep.icc`) and every other segment that is not an APPn or COM; from the first scan to
///   the EOI the bytes are copied as they are; bytes after the EOI are dropped.
/// - PNG: keeps the critical and the rendering chunks (IHDR, PLTE, IDAT, IEND, tRNS, cHRM,
///   gAMA, sBIT, sRGB, cICP, mDCV, cLLI, bKGD, hIST, pHYs, sPLT, the APNG chunks, iCCP with
///   `keep.icc`) and drops the rest.
/// - WebP: keeps VP8X, VP8, VP8L, ALPH, ANIM, ANMF and ICCP (with `keep.icc`), fixes the VP8X
///   flags and the RIFF size.
///
/// With `keep.orientation` the output has a new minimal EXIF block in the source byte order
/// when the input has an EXIF IFD0 Orientation (in a simple WebP file, whose VP8X chunk is
/// then added, only when the bitstream header gives the size).
///
/// The output buffer is charged to `limits.max_alloc` (input length plus 64 bytes); a copied
/// EXIF block to `limits.max_exif_bytes` and `limits.max_alloc`; segments and chunks to
/// `limits.max_segments`.
///
/// # Errors
///
/// [`Error::UnknownFormat`]; [`Error::Truncated`] when a JPEG ends before its first scan or
/// EOI, or a PNG or WebP file ends inside a chunk or before IEND; [`Error::Malformed`] for a
/// byte that is not a JPEG marker or a PNG chunk length over 2^31-1;
/// [`Error::LimitExceeded`] for `max_alloc`, `max_exif_bytes`, `max_segments` and the limits of
/// [`Exif::parse`]; [`Error::OutOfMemory`]; [`Error::Cancelled`].
pub fn strip(
    data: &[u8],
    keep: Keep,
    limits: &Limits,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    let budget = Budget::new(limits, cancel);
    match container::detect(data).ok_or(Error::UnknownFormat)? {
        Container::Jpeg => jpeg::strip(data, keep, limits, &budget),
        Container::Png => png::strip(data, keep, limits, &budget),
        Container::Webp => webp::strip(data, keep, limits, &budget),
    }
}

/// Sets the orientation values ExifTool reports to 1 and changes no other byte: the first value
/// of the EXIF IFD0 Orientation in place, in the width of its type (BYTE, SHORT, LONG or
/// signed; a PNG `eXIf` chunk gets a new CRC), and the text of the first top-level
/// `tiff:Orientation` of the XMP packet (the JPEG APP1 length, the PNG chunk length and CRC, the
/// WebP chunk size, pad byte and RIFF size follow). A chunk whose bytes do not change keeps its
/// CRC. When there is no orientation the output equals the input. A truncated file is
/// rewritten as far as it goes.
///
/// Only the first EXIF block and the first XMP packet are changed. Not changed: JPEG extended
/// XMP, a PNG compressed `iTXt` XMP packet, and a PNG ImageMagick raw EXIF profile (`tEXt`,
/// `zTXt`, `iTXt`); they are copied as they are.
///
/// The output buffer is charged to `limits.max_alloc` (input length plus 64 bytes), and so is
/// the scan of the XMP packet, which checks the cancel flag every 1024 nodes.
///
/// # Errors
///
/// [`Error::UnknownFormat`]; [`Error::Malformed`] when a changed JPEG APP1 or PNG chunk would
/// get too long for its length field; [`Error::LimitExceeded`] for `max_alloc`,
/// `max_segments`, `max_xmp_bytes` and `max_xmp_nodes` (the XMP packet that is scanned);
/// [`Error::OutOfMemory`]; [`Error::Cancelled`].
pub fn reset_orientation(
    data: &[u8],
    limits: &Limits,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    let budget = Budget::new(limits, cancel);
    match container::detect(data).ok_or(Error::UnknownFormat)? {
        Container::Jpeg => jpeg::reset(data, limits, &budget),
        Container::Png => png::reset(data, limits, &budget),
        Container::Webp => webp::reset(data, limits, &budget),
    }
}

/// The minimal EXIF block for the orientation of `block` (a raw EXIF block, which is
/// consumed and released from the budget); `None` when it has no orientation or is not valid.
pub(crate) fn kept_exif(
    block: Option<Vec<u8>>,
    limits: &Limits,
    budget: &Budget,
) -> Result<Option<[u8; MIN_EXIF_LEN]>, Error> {
    let Some(block) = block else {
        return Ok(None);
    };
    // Everything the parse charges is dropped here: give it back with the block.
    let mark = budget.used();
    let parsed = crate::soft(Exif::parse_with(&block, limits, budget));
    let kept =
        parsed.map(|p| p.and_then(|exif| exif.orientation().map(|o| (o, exif.byte_order()))));
    budget.reset_to(mark);
    budget.release(block.len());
    let Some((v, order)) = kept? else {
        return Ok(None);
    };
    let be = matches!(order, ByteOrder::Big);
    let u16b = |x: u16| if be { x.to_be_bytes() } else { x.to_le_bytes() };
    let u32b = |x: u32| if be { x.to_be_bytes() } else { x.to_le_bytes() };
    let mut t = [0u8; MIN_EXIF_LEN];
    let mut n = 0;
    let mut put = |b: &[u8]| {
        if let Some(d) = t.get_mut(n..n + b.len()) {
            d.copy_from_slice(b);
        }
        n += b.len();
    };
    let hdr: &[u8] = if be { b"MM\0*" } else { b"II*\0" };
    put(hdr);
    put(&u32b(8));
    put(&u16b(1));
    put(&u16b(0x0112));
    put(&u16b(3));
    put(&u32b(1));
    put(&u16b(v));
    put(&[0, 0]);
    put(&[0; 4]);
    Ok(Some(t))
}

/// Sets the first IFD0 Orientation value of the EXIF block to 1, in place, in the width of its
/// type. Returns whether a byte changed.
pub(crate) fn reset_exif_in_place(block: &mut [u8]) -> bool {
    let Some((range, order)) = crate::tiff::orientation_value_range(block) else {
        return false;
    };
    let Some(d) = block.get_mut(range) else {
        return false;
    };
    let one_at = match order {
        ByteOrder::Little => 0,
        ByteOrder::Big => d.len().saturating_sub(1),
    };
    let mut changed = false;
    for (i, b) in d.iter_mut().enumerate() {
        let v = u8::from(i == one_at);
        changed |= *b != v;
        *b = v;
    }
    changed
}
