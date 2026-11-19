//! Shared by the targets (`#[path = "../common.rs"] mod common;`): the small limits they run
//! under, and calls of every accessor with the invariants that hold for any input.
#![allow(dead_code)]

use std::mem::discriminant;
use std::sync::atomic::AtomicBool;

use exif::{Exif, IfdKind, Limits, Xmp};

/// Limits that keep one input well under the `-malloc_limit_mb=512` of the fuzz run, so an
/// allocation outside the limits is a crash.
pub fn small_limits() -> Limits {
    Limits {
        max_alloc: 64 << 20,
        max_threads: 1,
        max_exif_bytes: 1 << 20,
        max_xmp_bytes: 1 << 20,
        max_value_bytes: 16 << 20,
        max_xmp_nodes: 100_000,
        ..Limits::default()
    }
}

/// One constructor per IFD kind (a closure each, so `IfdKind` need not be `Copy`).
const KINDS: [fn() -> IfdKind; 5] = [
    || IfdKind::Ifd0,
    || IfdKind::Exif,
    || IfdKind::Gps,
    || IfdKind::Interop,
    || IfdKind::Ifd1,
];

/// Calls every accessor of `e`, parsed from `tiff`; `file` is the input file when `e` came from
/// `read` (a JPEG thumbnail may be read from the file past the APP1 block), else empty.
pub fn use_exif(e: &Exif, tiff: &[u8], file: &[u8]) {
    let _ = e.byte_order();
    for kind in KINDS {
        for en in e.entries(kind()) {
            assert!(e.get(kind(), en.tag).is_some(), "get misses an entry");
        }
    }
    let _ = (
        e.orientation(),
        e.make(),
        e.model(),
        e.lens_make(),
        e.lens_model(),
        e.capture_time(),
        e.exposure(),
        e.pixel_size(),
    );
    if let Some(g) = e.gps() {
        let _ = (g.latitude_deg(), g.longitude_deg(), g.altitude_m());
    }
    if let Some(m) = e.maker_note() {
        assert!(m.len() <= tiff.len(), "maker note longer than the block");
    }
    if let Some(t) = e.thumbnail() {
        assert!(
            t.len() <= tiff.len().max(file.len()),
            "thumbnail longer than the block and the file"
        );
    }
}

/// Calls every accessor of `x`.
pub fn use_xmp(x: &Xmp) {
    let _ = x.raw();
    for p in x.properties().iter().filter(|p| p.top_level) {
        assert!(x.get(&p.ns, &p.name).is_some(), "get misses a property");
    }
    let _ = (x.orientation(), x.create_date(), x.rating());
    let _ = x.hdrgm().count();
}

/// The jpeg, png and webp targets: `extract` and `read` on the raw input as the file.
pub fn container(data: &[u8]) {
    let lim = small_limits();
    let cancel = AtomicBool::new(false);
    let ex = exif::extract(data, &lim, &cancel);
    let rd = exif::read(data, &lim, &cancel);
    match (ex, rd) {
        (Ok(raw), rd) => {
            let d = exif::detect(data);
            assert!(
                d.is_some_and(|c| discriminant(&c) == discriminant(&raw.container)),
                "detect and extract disagree"
            );
            if let Some(e) = &raw.exif {
                assert!(e.len() <= lim.max_exif_bytes, "exif over max_exif_bytes");
            }
            let xmp_len = raw.xmp.as_ref().map_or(0, Vec::len)
                + raw.xmp_extended.as_ref().map_or(0, Vec::len);
            assert!(xmp_len <= lim.max_xmp_bytes, "xmp over max_xmp_bytes");
            if let Ok(m) = rd {
                assert_eq!(m.raw.exif, raw.exif, "read and extract differ");
                assert_eq!(m.raw.xmp, raw.xmp, "read and extract differ");
                assert_eq!(
                    m.raw.xmp_extended, raw.xmp_extended,
                    "read and extract differ"
                );
                assert_eq!(m.raw.icc, raw.icc, "read and extract differ");
                if let (Some(e), Some(t)) = (&m.exif, &m.raw.exif) {
                    use_exif(e, t, data);
                }
                if let Some(x) = &m.xmp {
                    use_xmp(x);
                }
            }
        }
        (Err(e1), Ok(_)) => panic!("extract failed ({e1}) but read succeeded"),
        (Err(e1), Err(e2)) => assert_eq!(e1, e2, "extract and read fail differently"),
    }
}
