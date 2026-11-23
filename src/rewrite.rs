//! Strip and rewrite: new file bytes with the metadata removed, or the orientation reset.

use std::sync::atomic::AtomicBool;

use crate::budget::Budget;
use crate::container::{self, Container, jpeg, png, webp};
use crate::{ByteOrder, Error, Exif, Limits};

/// What [`strip`] keeps besides the image data.
///
/// [`Keep::default()`] keeps everything that is not the metadata `strip` is for (every field
/// `true`); [`Keep::NONE`] keeps nothing that can be dropped. The default changed after v1: the v1
/// default was what `Keep::NONE` is now (no orientation, no ICC profile, no unknown JPEG
/// segment, PNG chunk or WebP chunk, nothing after a JPEG EOI, a PNG IEND or a WebP RIFF size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keep {
    /// Keep the EXIF IFD0 Orientation, in a new minimal EXIF block (nothing else of the EXIF),
    /// from the first EXIF block `strip` drops that has a valid one.
    pub orientation: bool,
    /// Keep the ICC profile, as stored.
    pub icc: bool,
    /// JPEG: keep the APPn segments that `strip` does not know as metadata, as stored and in
    /// place (for example an ISO 21496-1 gain map APP2, an APP0 that is not JFIF, a FlashPix
    /// APP2, an APP11 JUMBF box); PNG and WebP: the chunks `strip` does not know as metadata or
    /// image data, as stored and in place (for example a private chunk, a C2PA `caBX`).
    pub unknown: bool,
    /// JPEG: keep the MPF secondary images after the primary image (an Ultra HDR gain map, other
    /// MP images) and the APP2 `MPF` segment, whose offsets are fixed. Needs `trailing`.
    pub mpf_images: bool,
    /// Keep the bytes after the image, as stored: in a JPEG those after the EOI of the primary
    /// image (MPF images, a Motion Photo or Live Photo video, any other bytes), in a PNG those
    /// after IEND, in a WebP those after the end the RIFF size gives (and its pad byte). In a JPEG
    /// and a PNG they are not parsed: metadata in them stays. In a WebP they are read as chunks,
    /// as ExifTool reads them, and the `EXIF`, `XMP ` and `XMP\0` chunks among them are dropped.
    /// `false` drops all of them, the MPF images included.
    pub trailing: bool,
}

impl Keep {
    /// Keeps nothing that can be dropped: the v1 `Keep::default()`. A JPEG keeps only SOI, APP0
    /// `JFIF`, APP14 `Adobe`, the segments that are not APPn or COM, and the scans to the EOI.
    pub const NONE: Keep = Keep {
        orientation: false,
        icc: false,
        unknown: false,
        mpf_images: false,
        trailing: false,
    };
}

impl Default for Keep {
    /// Keeps everything that is not the metadata `strip` is for: every field `true`.
    fn default() -> Self {
        Keep {
            orientation: true,
            icc: true,
            unknown: true,
            mpf_images: true,
            trailing: true,
        }
    }
}

/// The length of the minimal EXIF block: TIFF header, IFD0 with one entry, next IFD 0.
pub(crate) const MIN_EXIF_LEN: usize = 26;

/// Removes the EXIF, XMP and IPTC metadata from a JPEG, PNG or WebP file, and with it what
/// `keep` does not name. The image data bytes are never changed.
///
/// [`Keep::default()`] keeps the orientation, the ICC profile, unknown JPEG segments and PNG /
/// WebP chunks, the MPF images and the bytes after the image (after the JPEG EOI, the PNG IEND,
/// the WebP RIFF size); [`Keep::NONE`] gives the v1 default output.
///
/// - JPEG: always drops the EXIF APP1 segments (`Exif\0` within the first bytes, as ExifTool
///   finds them), the XMP APP1 segments (standard, extended, and the other headers ExifTool
///   reads as XMP), APP13 Photoshop (`Photoshop 3.0\0`, `Adobe_Photoshop2.5:`) and COM. Before
///   the first scan it keeps SOI, APP0 `JFIF` (as stored, with its thumbnail if it has one),
///   APP14 `Adobe`, every segment that is not an APPn or COM, APP2 ICC (with `keep.icc`), the
///   APP2 `MPF` segments (with `keep.mpf_images` and `keep.trailing`) and every other APPn (with
///   `keep.unknown`), as stored and in place. From the first scan to the EOI of the primary
///   image the bytes are copied as they are. With `keep.trailing` the bytes after that EOI are
///   copied as they are, except, without `keep.mpf_images`, the MPF images (entries after the
///   first whose bytes lie after the EOI, start with `FF D8` and do not overlap an image before
///   them). The first MPF segment is fixed: the size of the primary image and the offsets of
///   the images that moved. When bytes after the EOI are kept and the standard XMP packet
///   describes them (`hdrgm:Version`, a `Container:Directory`, the Motion Photo or MicroVideo
///   fields of `http://ns.google.com/photos/1.0/camera/`), a new XMP packet with only those
///   fields goes after the minimal EXIF (or where it would go). It has no `hdrgm:Version`, and
///   the directory has no item for an image, when MPF images were dropped; when the directory
///   items cannot be matched to the dropped images (located from the end of the file by
///   `Item:Length` and `Item:Padding`), the packet has no directory.
/// - PNG: always drops `eXIf` (and the once-proposed `zxIf` / `zXIf`), `tIME` and the text
///   chunks `tEXt`, `zTXt` and `iTXt` (XMP, ImageMagick raw profiles, comments). Keeps the
///   critical and the rendering chunks (IHDR, PLTE, IDAT, IEND, tRNS, cHRM, gAMA, sBIT, sRGB,
///   cICP, mDCV, cLLI, bKGD, hIST, pHYs, sPLT, the APNG chunks), iCCP and the `Raw profile type
///   icc` / `icm` text chunks with `keep.icc`, and every other chunk with `keep.unknown`, as
///   stored and in place, and with `keep.trailing` the bytes after IEND, as they are. The new
///   `eXIf` goes right before the first iDOT or IDAT.
/// - WebP: always drops `EXIF`, `XMP ` and `XMP\0`, and a VP8X that is not the first one or is
///   shorter than 10 bytes. Keeps the first VP8X, VP8, VP8L, ALPH, ANIM and ANMF, ICCP with
///   `keep.icc`, and every other chunk with `keep.unknown`, as stored and in place with its pad
///   byte. Fixes the VP8X flags and the RIFF size. With `keep.trailing` the bytes after the end
///   the RIFF size gives (and its pad byte) follow, as they are but for the `EXIF`, `XMP ` and
///   `XMP\0` chunks among them (read as chunks up to the first bytes that are not a whole one).
///
/// With `keep.orientation` the output has a new minimal EXIF block in the source byte order
/// when an EXIF block that `strip` drops has a valid IFD0 Orientation: the first such block (in
/// a JPEG an APP1 that `strip` drops as EXIF, the first `Exif\0\0` one read joined as
/// [`extract`](crate::extract) gives it; in a WebP an `EXIF` chunk within the RIFF size, else
/// one after it), so also one that [`read`](crate::read) does not report. In a simple WebP file
/// the VP8X chunk is then added first, only when the bitstream header gives the size; a WebP
/// whose RIFF data has no VP8X and no VP8 or VP8L gets no minimal EXIF.
///
/// The output buffer is charged to `limits.max_alloc` (input length plus 64 bytes, plus the new
/// XMP packet); a copied EXIF block to `limits.max_exif_bytes` and `limits.max_alloc`; segments
/// and chunks to `limits.max_segments`. The scan of the JPEG XMP packet (only when bytes after
/// the EOI are kept) and the offset map of the MPF fix-up are charged to `limits.max_alloc`; the
/// scan checks the cancel flag every 1024 nodes.
///
/// # Errors
///
/// [`Error::UnknownFormat`]; [`Error::Truncated`] when a JPEG ends before its first scan or
/// EOI, or a PNG or WebP file ends inside a chunk or before IEND; [`Error::Malformed`] for a
/// byte that is not a JPEG marker or a PNG chunk length over 2^31-1;
/// [`Error::LimitExceeded`] for `max_alloc`, `max_exif_bytes`, `max_segments`, `max_xmp_bytes`,
/// `max_xmp_nodes` and the limits of [`Exif::parse`]; [`Error::OutOfMemory`];
/// [`Error::Cancelled`].
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
/// Only the first EXIF block and the first XMP packet are changed. A JPEG EXIF block continued
/// in the APP1 segments right after it is read joined, as [`extract`](crate::extract) gives it,
/// and the value is written back into the segment that holds it. Not changed: JPEG extended
/// XMP, a PNG compressed `iTXt` XMP packet, and a PNG ImageMagick raw EXIF profile (`tEXt`,
/// `zTXt`, `iTXt`); they are copied as they are.
///
/// Every byte of a JPEG is kept, those after the EOI too (MPF images, a Motion Photo video).
/// When the XMP value changes length (`" 6"` becomes `1`), the first APP2 `MPF` segment before
/// the first scan is fixed: the offsets of the images after the value move with it, and the
/// size of the primary image is set to its new length.
///
/// The output buffer is charged to `limits.max_alloc` (input length plus 64 bytes), and so are
/// the scan of the XMP packet, which checks the cancel flag every 1024 nodes, and the offset map
/// of the MPF fix-up.
///
/// # Errors
///
/// [`Error::UnknownFormat`]; [`Error::Malformed`] when a changed JPEG APP1 or PNG chunk would
/// get too long for its length field, or a WebP chunk size or RIFF size would leave the `u32`
/// range; [`Error::LimitExceeded`] for `max_alloc`, `max_segments`, `max_exif_bytes` (the
/// joined JPEG EXIF block), `max_xmp_bytes` and `max_xmp_nodes` (the XMP packet that is scanned);
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
    // The block was charged by capacity (a joined JPEG block can hold more than its length).
    budget.release(block.capacity());
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
