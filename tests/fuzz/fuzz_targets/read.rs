#![no_main]
use libfuzzer_sys::fuzz_target;
use std::sync::atomic::AtomicBool;

#[path = "../common.rs"]
mod common;

// data = the file; exif::read, then every accessor of what it parsed.
fuzz_target!(|data: &[u8]| {
    let cancel = AtomicBool::new(false);
    if let Ok(m) = exif::read(data, &common::small_limits(), &cancel) {
        if let (Some(e), Some(t)) = (&m.exif, &m.raw.exif) {
            common::use_exif(e, t, data);
        }
        if let Some(x) = &m.xmp {
            common::use_xmp(x);
        }
    }
});
