use std::fmt;

/// The [`Limits`](crate::Limits) field that a call would pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Limit {
    /// [`Limits::max_alloc`](crate::Limits::max_alloc).
    Alloc,
    /// [`Limits::max_exif_bytes`](crate::Limits::max_exif_bytes).
    ExifBytes,
    /// [`Limits::max_xmp_bytes`](crate::Limits::max_xmp_bytes).
    XmpBytes,
    /// [`Limits::max_ifds`](crate::Limits::max_ifds).
    Ifds,
    /// [`Limits::max_entries`](crate::Limits::max_entries).
    Entries,
    /// [`Limits::max_total_entries`](crate::Limits::max_total_entries).
    TotalEntries,
    /// [`Limits::max_depth`](crate::Limits::max_depth).
    Depth,
    /// [`Limits::max_value_bytes`](crate::Limits::max_value_bytes).
    ValueBytes,
    /// [`Limits::max_segments`](crate::Limits::max_segments).
    Segments,
    /// [`Limits::max_xmp_nodes`](crate::Limits::max_xmp_nodes).
    XmpNodes,
}

/// Every error of this crate. Limit errors are one variant with a [`Limit`] so a caller can
/// match on the limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The input is not a JPEG, PNG or WebP file (or, for [`Exif::parse`](crate::Exif::parse),
    /// not a TIFF header).
    UnknownFormat,
    /// The input ends inside a structure that the operation needs (for example a JPEG segment
    /// before the first scan, when the call has to rewrite the file).
    Truncated,
    /// A structure is invalid. The text names it, for example `"png chunk crc"`. It is a short
    /// stable token, not a sentence.
    Malformed(&'static str),
    /// A [`Limits`](crate::Limits) field would be passed.
    LimitExceeded(Limit),
    /// The allocator refused a size that is within the limits.
    OutOfMemory,
    /// The cancel flag was set.
    Cancelled,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFormat => f.write_str("unknown format"),
            Self::Truncated => f.write_str("truncated input"),
            Self::Malformed(what) => write!(f, "malformed {what}"),
            Self::LimitExceeded(limit) => write!(f, "limit exceeded: {limit:?}"),
            Self::OutOfMemory => f.write_str("out of memory"),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// A short stable token per variant (`unknown-format`, `truncated`, `malformed`,
    /// `limit-<field>`, `out-of-memory`, `cancelled`). The parity oracle writes it.
    pub fn class(&self) -> &'static str {
        match self {
            Self::UnknownFormat => "unknown-format",
            Self::Truncated => "truncated",
            Self::Malformed(_) => "malformed",
            Self::LimitExceeded(limit) => match limit {
                Limit::Alloc => "limit-alloc",
                Limit::ExifBytes => "limit-exif-bytes",
                Limit::XmpBytes => "limit-xmp-bytes",
                Limit::Ifds => "limit-ifds",
                Limit::Entries => "limit-entries",
                Limit::TotalEntries => "limit-total-entries",
                Limit::Depth => "limit-depth",
                Limit::ValueBytes => "limit-value-bytes",
                Limit::Segments => "limit-segments",
                Limit::XmpNodes => "limit-xmp-nodes",
            },
            Self::OutOfMemory => "out-of-memory",
            Self::Cancelled => "cancelled",
        }
    }
}
