#![no_main]
use exif::{Error, IfdKind, Keep};
use libfuzzer_sys::fuzz_target;
use std::sync::atomic::AtomicBool;

#[path = "../common.rs"]
mod common;

// data = [selector][file]. Selector bit 0 is Keep::orientation, bit 1 Keep::icc, bit 2 picks
// reset_orientation instead of strip. The output is the oracle: it must read as a file (unless the
// input did not), and a stripped file has no XMP, no ICC unless kept, and no EXIF unless the
// orientation is kept, then only IFD0 Orientation.
fuzz_target!(|data: &[u8]| {
    let Some((&sel, file)) = data.split_first() else {
        return;
    };
    let keep = Keep {
        orientation: sel & 1 != 0,
        icc: sel & 2 != 0,
    };
    let reset = sel & 4 != 0;
    let lim = common::small_limits();
    let cancel = AtomicBool::new(false);
    let input_reads = exif::read(file, &lim, &cancel).is_ok();
    let out = if reset {
        exif::reset_orientation(file, &lim, &cancel)
    } else {
        exif::strip(file, keep, &lim, &cancel)
    };
    let Ok(out) = out else {
        return;
    };
    let m = match exif::read(&out, &lim, &cancel) {
        Ok(m) => m,
        Err(Error::UnknownFormat | Error::Truncated | Error::Malformed(_)) => {
            assert!(
                !input_reads,
                "the input reads as a file but the output does not"
            );
            return;
        }
        Err(_) => return,
    };
    if reset {
        // Orientation is already 1 everywhere, so a second reset changes nothing.
        if let Ok(again) = exif::reset_orientation(&out, &lim, &cancel) {
            assert_eq!(again, out, "reset_orientation is not idempotent");
        }
        return;
    }
    assert!(m.raw.xmp.is_none(), "stripped output has XMP");
    assert!(
        m.raw.xmp_extended.is_none(),
        "stripped output has extended XMP"
    );
    assert!(keep.icc || m.raw.icc.is_none(), "stripped output has ICC");
    assert!(
        keep.orientation || m.raw.exif.is_none(),
        "stripped output has EXIF"
    );
    if let Some(e) = &m.exif {
        assert!(e.entries(IfdKind::Ifd0).iter().all(|n| n.tag == 0x0112));
        for kind in [IfdKind::Exif, IfdKind::Gps, IfdKind::Interop, IfdKind::Ifd1] {
            assert!(
                e.entries(kind).is_empty(),
                "stripped output has a second IFD"
            );
        }
    }
});
