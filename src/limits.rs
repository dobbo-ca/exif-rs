/// Caps on one call. Every public entry point takes `&Limits` and a cancel flag; there is no
/// unlimited entry point. The defaults are finite and large enough for real camera and phone
/// files (an EXIF block of a phone photo is under 64 KiB; a maker note can be some 100 KiB).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The peak working memory of one call, in bytes (default 256 MiB): every buffer whose size
    /// comes from the input (copies of the EXIF and XMP blocks, parsed values, inflated PNG text,
    /// the output of a rewrite) is charged before it is allocated. More is
    /// [`Error::LimitExceeded`](crate::Error::LimitExceeded)`(`[`Limit::Alloc`](crate::Limit::Alloc)`)`.
    pub max_alloc: usize,
    /// The most OS threads one call uses, including the calling thread (default
    /// `std::thread::available_parallelism()`). This crate never spawns a thread: every call
    /// runs on the calling thread, for every value. The field exists so that a caller sets the
    /// same `Limits` fields on every dobbo-ca crate.
    pub max_threads: usize,
    /// The largest EXIF block, in bytes (default 16 MiB). [`Limit::ExifBytes`](crate::Limit::ExifBytes).
    pub max_exif_bytes: usize,
    /// The largest XMP packet, standard and extended together, in bytes (default 16 MiB).
    /// [`Limit::XmpBytes`](crate::Limit::XmpBytes).
    pub max_xmp_bytes: usize,
    /// The most IFDs one EXIF block may hold, all kinds together (default 64).
    /// [`Limit::Ifds`](crate::Limit::Ifds).
    pub max_ifds: usize,
    /// The most entries in one IFD (default 4096). [`Limit::Entries`](crate::Limit::Entries).
    pub max_entries: usize,
    /// The most entries in all IFDs together (default 65536).
    /// [`Limit::TotalEntries`](crate::Limit::TotalEntries).
    pub max_total_entries: usize,
    /// The deepest IFD nesting: IFD0 is depth 0, its Exif IFD depth 1, the Interop IFD depth 2
    /// (default 8). [`Limit::Depth`](crate::Limit::Depth).
    pub max_depth: usize,
    /// The most value bytes read from all entries together (default 64 MiB).
    /// [`Limit::ValueBytes`](crate::Limit::ValueBytes).
    pub max_value_bytes: usize,
    /// The most JPEG segments, PNG chunks or RIFF chunks walked in one file (default 100 000).
    /// [`Limit::Segments`](crate::Limit::Segments).
    pub max_segments: usize,
    /// The most XML elements and attributes parsed in one XMP packet (default 1 000 000).
    /// [`Limit::XmpNodes`](crate::Limit::XmpNodes).
    pub max_xmp_nodes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_alloc: 256 << 20,
            max_threads: std::thread::available_parallelism().map_or(1, usize::from),
            max_exif_bytes: 16 << 20,
            max_xmp_bytes: 16 << 20,
            max_ifds: 64,
            max_entries: 4096,
            max_total_entries: 65536,
            max_depth: 8,
            max_value_bytes: 64 << 20,
            max_segments: 100_000,
            max_xmp_nodes: 1_000_000,
        }
    }
}
