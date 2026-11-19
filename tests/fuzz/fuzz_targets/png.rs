#![no_main]
use libfuzzer_sys::fuzz_target;

#[path = "../common.rs"]
mod common;

// data = the file (png seeds start with its magic). extract and read agree.
fuzz_target!(|data: &[u8]| common::container(data));
