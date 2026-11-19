#![no_main]
use libfuzzer_sys::fuzz_target;
use std::sync::atomic::AtomicBool;

#[path = "../common.rs"]
mod common;

// data = the TIFF block. Every accessor runs on what parses.
fuzz_target!(|data: &[u8]| {
    let cancel = AtomicBool::new(false);
    if let Ok(e) = exif::Exif::parse(data, &common::small_limits(), &cancel) {
        common::use_exif(&e, data, &[]);
    }
});
