#![no_main]
use libfuzzer_sys::fuzz_target;
use std::sync::atomic::AtomicBool;

#[path = "../common.rs"]
mod common;

// data = [selector][packet bytes]. An even selector parses the rest as the standard packet alone;
// an odd one splits the rest at (selector >> 1) / 128 of its length into packet + extended.
fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else {
        return;
    };
    let cancel = AtomicBool::new(false);
    let lim = common::small_limits();
    let parsed = if sel & 1 == 0 {
        exif::Xmp::parse(rest, None, &lim, &cancel)
    } else {
        let mid = (usize::from(sel >> 1) * rest.len() / 128).min(rest.len());
        let (packet, extended) = rest.split_at(mid);
        exif::Xmp::parse(packet, Some(extended), &lim, &cancel)
    };
    if let Ok(x) = parsed {
        common::use_xmp(&x);
    }
});
