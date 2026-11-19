//! EXIF and XMP metadata: read it, strip it, or rewrite it, in JPEG, PNG and WebP files.
//!
//! The container layer ([`extract`]) finds the EXIF block, the XMP packet and the ICC profile;
//! [`Exif::parse`] and [`Xmp::parse`] take those bytes from any container (HEIF and AVIF will give
//! them through isobmff-rs). [`strip`] and [`reset_orientation`] write a new file and never change
//! the image data bytes. Offsets in the input are never trusted: the IFD walk is bounded (cycle
//! detection, entry, depth and byte limits) and no call panics on any input.
//!
//! # Example
//!
//! ```
//! use std::sync::atomic::AtomicBool;
//! use exif::{Keep, Limits};
//!
//! let limits = Limits { max_threads: 1, max_alloc: 64 << 20, ..Limits::default() };
//! let cancel = AtomicBool::new(false);
//! # let file: &[u8] = &[0xFF, 0xD8, 0xFF, 0xD9];
//! let meta = exif::read(file, &limits, &cancel)?;
//! if let Some(exif) = &meta.exif {
//!     println!("orientation {:?}, taken {:?}", exif.orientation(), exif.capture_time());
//! }
//! let clean = exif::strip(file, Keep { orientation: true, icc: true }, &limits, &cancel)?;
//! # assert_eq!(clean, file);
//! # Ok::<(), exif::Error>(())
//! ```
//!
//! # Resource limits
//!
//! Every entry point takes [`Limits`] and a cancel flag (`&AtomicBool`).
//!
//! - `max_alloc` (default 256 MiB) is the enforced peak of the working memory of one call: each
//!   buffer whose size comes from the input is charged before it is allocated. More is
//!   [`Error::LimitExceeded`]`(`[`Limit::Alloc`]`)`.
//! - `max_threads` (default `available_parallelism`): this crate never spawns a thread, so every
//!   call runs on the calling thread for every value, including 1.
//! - The structural caps (`max_exif_bytes`, `max_xmp_bytes`, `max_ifds`, `max_entries`,
//!   `max_total_entries`, `max_depth`, `max_value_bytes`, `max_segments`, `max_xmp_nodes`) each
//!   give [`Error::LimitExceeded`] with their own [`Limit`].
//! - Set the cancel flag from another thread to stop a call: it returns [`Error::Cancelled`]. The
//!   flag is checked per IFD, per JPEG segment, PNG or RIFF chunk, and every 1024 XML nodes.

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::todo,
        clippy::unimplemented
    )
)]

use std::sync::atomic::AtomicBool;

mod budget;
mod container;
mod crc32;
mod error;
mod exif;
mod limits;
mod rewrite;
mod tiff;
mod xmp;

pub use container::{Container, Raw, detect, extract};
pub use error::{Error, Limit};
pub use exif::{
    ByteOrder, CaptureTime, Entry, Exif, Exposure, Gps, IfdKind, Rational, SRational, Value,
};
pub use limits::Limits;
pub use rewrite::{Keep, reset_orientation, strip};
pub use xmp::{Property, Xmp};

use budget::Budget;

/// What [`read`] found in a file: the raw blocks and the parsed EXIF and XMP.
#[derive(Debug, Clone)]
pub struct Metadata {
    /// The container and the raw EXIF, XMP and ICC bytes.
    pub raw: Raw,
    /// The parsed EXIF block; `None` when the file has none or its TIFF header is invalid.
    pub exif: Option<Exif>,
    /// The parsed XMP packet; `None` when the file has none or it is not XML.
    pub xmp: Option<Xmp>,
}

/// Reads the metadata of a JPEG, PNG or WebP file: [`extract`], then [`Exif::parse`] and
/// [`Xmp::parse`] on what it found, with one budget for the whole call.
///
/// A malformed EXIF block or XMP packet gives `None` for that part (its raw bytes stay in
/// [`Metadata::raw`]); the other part is still read.
///
/// # Errors
///
/// [`Error::UnknownFormat`] when `data` is not JPEG, PNG or WebP; [`Error::Malformed`] when
/// the container structure is invalid; [`Error::LimitExceeded`] for any [`Limits`] field;
/// [`Error::OutOfMemory`]; [`Error::Cancelled`].
pub fn read(data: &[u8], limits: &Limits, cancel: &AtomicBool) -> Result<Metadata, Error> {
    let budget = Budget::new(limits, cancel);
    let raw = container::extract_with(data, limits, &budget)?;
    let exif = match raw.exif.as_deref() {
        Some(tiff) => soft(Exif::parse_with(tiff, limits, &budget))?,
        None => None,
    };
    let xmp = match raw.xmp.as_deref() {
        Some(packet) => soft(Xmp::parse_with(
            packet,
            raw.xmp_extended.as_deref(),
            limits,
            &budget,
        ))?,
        None => None,
    };
    Ok(Metadata { raw, exif, xmp })
}

/// A format error of one part becomes `None`; limits, cancel and OOM stay errors.
fn soft<T>(r: Result<T, Error>) -> Result<Option<T>, Error> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(Error::Malformed(_) | Error::Truncated | Error::UnknownFormat) => Ok(None),
        Err(e) => Err(e),
    }
}
