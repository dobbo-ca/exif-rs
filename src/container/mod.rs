//! Container detection and the raw EXIF, XMP and ICC blocks of JPEG, PNG and WebP files.

use std::ops::Range;
use std::sync::atomic::AtomicBool;

use crate::budget::Budget;
use crate::{Error, Limit, Limits};

pub(crate) mod jpeg;
pub(crate) mod png;
pub(crate) mod webp;

/// The container format of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// JPEG (JFIF, Exif, ...).
    Jpeg,
    /// PNG.
    Png,
    /// WebP (RIFF).
    Webp,
}

/// The raw metadata blocks of a file, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Raw {
    /// The container the blocks came from.
    pub container: Container,
    /// The EXIF block, starting at the TIFF header (`Exif\0\0` removed).
    pub exif: Option<Vec<u8>>,
    /// The standard XMP packet, as stored.
    pub xmp: Option<Vec<u8>>,
    /// JPEG extended XMP, the chunks put together by offset; `None` in other containers.
    pub xmp_extended: Option<Vec<u8>>,
    /// The ICC profile (JPEG APP2 chunks joined, PNG iCCP inflated, WebP ICCP).
    pub icc: Option<Vec<u8>>,
}

/// Which blocks a walk has to collect.
#[derive(Clone, Copy)]
pub(crate) struct Want {
    pub exif: bool,
    pub xmp: bool,
    pub icc: bool,
}

impl Want {
    pub(crate) const ALL: Self = Self {
        exif: true,
        xmp: true,
        icc: true,
    };
    pub(crate) const EXIF: Self = Self {
        exif: true,
        xmp: false,
        icc: false,
    };
}

pub(crate) const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
pub(crate) const EXIF_PREFIX: &[u8] = b"Exif\0\0";
/// Room for what a rewrite adds (a minimal EXIF block and a VP8X chunk need under 64 bytes).
const OUT_SLACK: usize = 64;

/// Detects the container by its magic bytes only.
#[must_use]
pub fn detect(data: &[u8]) -> Option<Container> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Container::Jpeg)
    } else if data.starts_with(&PNG_SIG) {
        Some(Container::Png)
    } else if data.starts_with(b"RIFF") && data.get(8..12).is_some_and(|s| s == b"WEBP") {
        Some(Container::Webp)
    } else {
        None
    }
}

/// Finds the raw EXIF, XMP and ICC blocks of a JPEG, PNG or WebP file.
///
/// A file that ends early still gives the blocks that were read. The walk is bounded by
/// `limits.max_segments`; a copied block by `limits.max_exif_bytes` or `limits.max_xmp_bytes`;
/// every copy is charged to `limits.max_alloc`. The cancel flag is checked per segment or chunk.
///
/// # Errors
///
/// [`Error::UnknownFormat`] when `data` is not JPEG, PNG or WebP;
/// [`Error::LimitExceeded`] for `max_segments`, `max_exif_bytes`, `max_xmp_bytes` and
/// `max_alloc`; [`Error::OutOfMemory`]; [`Error::Cancelled`].
pub fn extract(data: &[u8], limits: &Limits, cancel: &AtomicBool) -> Result<Raw, Error> {
    let budget = Budget::new(limits, cancel);
    extract_with(data, limits, &budget)
}

pub(crate) fn extract_with(data: &[u8], limits: &Limits, budget: &Budget) -> Result<Raw, Error> {
    match detect(data).ok_or(Error::UnknownFormat)? {
        Container::Jpeg => jpeg::extract(data, limits, budget, Want::ALL),
        Container::Png => png::extract(data, limits, budget, Want::ALL),
        Container::Webp => webp::extract(data, limits, budget, Want::ALL),
    }
}

/// Counts the segments or chunks of one walk and checks the cancel flag for each.
pub(crate) struct Walk<'a, 'b> {
    n: usize,
    limits: &'a Limits,
    budget: &'a Budget<'b>,
}

impl<'a, 'b> Walk<'a, 'b> {
    pub(crate) fn new(limits: &'a Limits, budget: &'a Budget<'b>) -> Self {
        Self {
            n: 0,
            limits,
            budget,
        }
    }

    pub(crate) fn tick(&mut self) -> Result<(), Error> {
        self.budget.check_cancel()?;
        self.n = self.n.saturating_add(1);
        if self.n > self.limits.max_segments {
            return Err(Error::LimitExceeded(Limit::Segments));
        }
        Ok(())
    }
}

pub(crate) fn be16(d: &[u8], at: usize) -> Option<u16> {
    let b: [u8; 2] = d.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_be_bytes(b))
}

pub(crate) fn be32(d: &[u8], at: usize) -> Option<u32> {
    let b: [u8; 4] = d.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(b))
}

pub(crate) fn le16(d: &[u8], at: usize) -> Option<u16> {
    let b: [u8; 2] = d.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(b))
}

pub(crate) fn le32(d: &[u8], at: usize) -> Option<u32> {
    let b: [u8; 4] = d.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(b))
}

/// `b` without a leading `Exif\0\0`, when it has one.
pub(crate) fn exif_body(b: &[u8]) -> &[u8] {
    b.strip_prefix(EXIF_PREFIX).unwrap_or(b)
}

/// A charged copy of `src`, or `LimitExceeded(limit)` when it is longer than `cap`.
pub(crate) fn capped_copy(
    budget: &Budget,
    src: &[u8],
    cap: usize,
    limit: Limit,
) -> Result<Vec<u8>, Error> {
    if src.len() > cap {
        return Err(Error::LimitExceeded(limit));
    }
    budget.copy(src)
}

/// A charged buffer of `n` zero bytes.
pub(crate) fn zeroed(budget: &Budget, n: usize) -> Result<Vec<u8>, Error> {
    let mut v = budget.vec::<u8>(n)?;
    v.resize(n, 0);
    Ok(v)
}

/// A charged empty output buffer for a rewrite of `input_len` bytes (input plus slack).
pub(crate) fn out_vec(budget: &Budget, input_len: usize) -> Result<Vec<u8>, Error> {
    budget.vec(input_len.saturating_add(OUT_SLACK))
}

/// The byte range of the `tiff:Orientation` value in `packet`, when it is valid for `packet`.
pub(crate) fn xmp_span(packet: &[u8], limits: &Limits) -> Option<Range<usize>> {
    let r = crate::xmp::orientation_value_span(packet, limits)?;
    packet.get(r.clone())?;
    Some(r)
}

/// Replaces the bytes at `span` of `out` by `1`. `out` has room for one more byte.
pub(crate) fn splice_one(out: &mut Vec<u8>, span: Range<usize>) {
    out.splice(span, [b'1']).for_each(drop);
}
