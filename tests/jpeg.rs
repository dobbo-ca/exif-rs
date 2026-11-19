//! JPEG: extract.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};

use common::*;
use exif::{Container, Error, Limit, Limits, detect, extract};

const GUID_A: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const GUID_B: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

fn std_xmp_attr(guid: &str) -> Vec<u8> {
    format!("<x:xmpmeta><rdf:Description xmpNote:HasExtendedXMP=\"{guid}\"/></x:xmpmeta>")
        .into_bytes()
}

#[test]
fn exif_both_byte_orders() {
    for be in [false, true] {
        let t = tiff(be, &[short(be, 0x0112, 6)]);
        let f = jpeg(&[app0_jfif(), app1_exif(&t)]);
        assert_eq!(detect(&f), Some(Container::Jpeg));
        let raw = extract(&f, &limits(), &no_cancel()).unwrap();
        assert_eq!(raw.container, Container::Jpeg);
        assert_eq!(raw.exif.as_deref(), Some(&t[..]));
        assert!(raw.xmp.is_none() && raw.xmp_extended.is_none() && raw.icc.is_none());
    }
}

#[test]
fn thumbnail_past_the_app1_segment_is_read_from_the_file() {
    // IFD0 (no entries) -> IFD1 at 14 with the thumbnail offset 44 and length 4; the block holds
    // only FF D8 of it, the rest is the start of the next segment (FF DB), as in some Sony and
    // Sigma files. ExifTool reads the thumbnail from the file.
    let mut t = b"II*\0\x08\0\0\0\0\0\x0e\0\0\0\x02\0".to_vec();
    for (tag, v) in [(0x0201u16, 44u32), (0x0202, 4)] {
        t.extend(tag.to_le_bytes());
        t.extend(4u16.to_le_bytes());
        t.extend(1u32.to_le_bytes());
        t.extend(v.to_le_bytes());
    }
    t.extend([0, 0, 0, 0, 0xFF, 0xD8]);
    assert_eq!(t.len(), 46);
    let f = jpeg(&[app0_jfif(), app1_exif(&t), dqt()]);
    let meta = exif::read(&f, &limits(), &no_cancel()).unwrap();
    let thumb = meta.exif.as_ref().unwrap().thumbnail();
    assert_eq!(thumb, Some(&[0xFF, 0xD8, 0xFF, 0xDB][..]));
    // Past the end of the file: no thumbnail; the block alone has none either.
    let mut end = jpeg(&[app0_jfif(), app1_exif(&t)]);
    end.truncate(end.len() - SCAN.len() + 1);
    let meta = exif::read(&end, &limits(), &no_cancel()).unwrap();
    assert_eq!(meta.exif.unwrap().thumbnail(), None);
    let parsed = exif::Exif::parse(&t, &limits(), &no_cancel()).unwrap();
    assert_eq!(parsed.thumbnail(), None);
}

#[test]
fn first_exif_and_first_xmp_win() {
    let t1 = tiff(false, &[short(false, 0x0112, 1)]);
    let t2 = tiff(false, &[short(false, 0x0112, 2)]);
    let f = jpeg(&[
        app1_exif(&t1),
        app1_exif(&t2),
        app1_xmp(b"<one/>"),
        app1_xmp(b"<two/>"),
    ]);
    let raw = extract(&f, &limits(), &no_cancel()).unwrap();
    assert_eq!(raw.exif, Some(t1));
    assert_eq!(raw.xmp.as_deref(), Some(&b"<one/>"[..]));
}

#[test]
fn xmp_standard_packet() {
    let f = jpeg(&[app1_xmp(xmp_plain().as_bytes())]);
    let raw = extract(&f, &limits(), &no_cancel()).unwrap();
    assert_eq!(raw.xmp.as_deref(), Some(xmp_plain().as_bytes()));
    assert!(raw.exif.is_none());
}

#[test]
fn icc_chunks_out_of_order() {
    let f = jpeg(&[
        app2_icc(2, 3, b"bb"),
        app2_icc(3, 3, b"cc"),
        app2_icc(1, 3, b"aa"),
    ]);
    let raw = extract(&f, &limits(), &no_cancel()).unwrap();
    assert_eq!(raw.icc.as_deref(), Some(&b"aabbcc"[..]));
}

#[test]
fn icc_with_a_missing_chunk_is_none() {
    let f = jpeg(&[app2_icc(1, 3, b"aa"), app2_icc(3, 3, b"cc")]);
    let raw = extract(&f, &limits(), &no_cancel()).unwrap();
    assert!(raw.icc.is_none());
}

#[test]
fn extended_xmp_attribute_form_wrong_guid_ignored() {
    let f = jpeg(&[
        app1_xmp(&std_xmp_attr(GUID_A)),
        app1_ext(GUID_A, 10, 5, b"56789"),
        app1_ext(GUID_B, 10, 0, b"XXXXXXXXXX"),
        app1_ext(GUID_A, 10, 0, b"01234"),
    ]);
    let raw = extract(&f, &limits(), &no_cancel()).unwrap();
    assert_eq!(raw.xmp_extended.as_deref(), Some(&b"0123456789"[..]));
}

#[test]
fn extended_xmp_element_form() {
    let std = format!(
        "<x:xmpmeta><rdf:Description><xmpNote:HasExtendedXMP>{GUID_A}</xmpNote:HasExtendedXMP></rdf:Description></x:xmpmeta>"
    );
    let f = jpeg(&[app1_xmp(std.as_bytes()), app1_ext(GUID_A, 4, 0, b"abcd")]);
    let raw = extract(&f, &limits(), &no_cancel()).unwrap();
    assert_eq!(raw.xmp_extended.as_deref(), Some(&b"abcd"[..]));
}

#[test]
fn extended_xmp_only_wrong_guid_is_none() {
    let f = jpeg(&[
        app1_xmp(&std_xmp_attr(GUID_A)),
        app1_ext(GUID_B, 4, 0, b"abcd"),
    ]);
    let raw = extract(&f, &limits(), &no_cancel()).unwrap();
    assert!(raw.xmp_extended.is_none());
}

#[test]
fn extended_xmp_without_has_extended_is_none() {
    let f = jpeg(&[
        app1_xmp(xmp_plain().as_bytes()),
        app1_ext(GUID_A, 4, 0, b"abcd"),
    ]);
    let raw = extract(&f, &limits(), &no_cancel()).unwrap();
    assert!(raw.xmp_extended.is_none());
}

#[test]
fn extended_xmp_chunk_past_the_declared_length_is_ignored() {
    let f = jpeg(&[
        app1_xmp(&std_xmp_attr(GUID_A)),
        app1_ext(GUID_A, 4, 2, b"abcd"),
        app1_ext(GUID_A, 4, 0, b"wxyz"),
    ]);
    let raw = extract(&f, &limits(), &no_cancel()).unwrap();
    assert_eq!(raw.xmp_extended.as_deref(), Some(&b"wxyz"[..]));
}

#[test]
fn not_a_jpeg() {
    assert_eq!(
        extract(b"hello world", &limits(), &no_cancel()),
        Err(Error::UnknownFormat)
    );
    assert_eq!(detect(b"hello world"), None);
}

#[test]
fn eoi_without_scan_and_fill_bytes() {
    let t = tiff(false, &[short(false, 0x0112, 6)]);
    let mut f = vec![0xFF, 0xD8, 0xFF, 0xFF];
    f.extend_from_slice(&app1_exif(&t)[1..]);
    f.extend_from_slice(&[0xFF, 0xD9]);
    let raw = extract(&f, &limits(), &no_cancel()).unwrap();
    assert_eq!(raw.exif, Some(t));
}

#[test]
fn truncated_at_every_length() {
    let t = tiff(false, &[short(false, 0x0112, 6)]);
    let f = jpeg(&[
        app0_jfif(),
        app1_exif(&t),
        app1_xmp(xmp_plain().as_bytes()),
        app2_icc(1, 1, b"icc"),
        dqt(),
    ]);
    let exif_end = 2 + app0_jfif().len() + app1_exif(&t).len();
    for n in 0..=f.len() {
        let r = extract(&f[..n], &limits(), &no_cancel());
        if n < 3 {
            assert_eq!(r, Err(Error::UnknownFormat), "n={n}");
        } else {
            let raw = r.unwrap();
            assert_eq!(raw.exif.is_some(), n >= exif_end, "n={n}");
        }
    }
}

#[test]
fn limit_max_segments() {
    let t = tiff(false, &[short(false, 0x0112, 6)]);
    let f = jpeg(&[app0_jfif(), app1_exif(&t), dqt()]);
    let l = Limits {
        max_segments: 2,
        ..limits()
    };
    assert_eq!(
        extract(&f, &l, &no_cancel()),
        Err(Error::LimitExceeded(Limit::Segments))
    );
    let l = Limits {
        max_segments: 100,
        ..limits()
    };
    assert!(extract(&f, &l, &no_cancel()).is_ok());
}

#[test]
fn limit_max_exif_bytes() {
    let t = tiff(false, &[short(false, 0x0112, 6)]);
    let f = jpeg(&[app1_exif(&t)]);
    let l = Limits {
        max_exif_bytes: t.len() - 1,
        ..limits()
    };
    assert_eq!(
        extract(&f, &l, &no_cancel()),
        Err(Error::LimitExceeded(Limit::ExifBytes))
    );
    let l = Limits {
        max_exif_bytes: t.len(),
        ..limits()
    };
    assert!(extract(&f, &l, &no_cancel()).is_ok());
}

#[test]
fn limit_max_xmp_bytes() {
    let std = std_xmp_attr(GUID_A);
    let f = jpeg(&[app1_xmp(&std)]);
    let l = Limits {
        max_xmp_bytes: std.len() - 1,
        ..limits()
    };
    assert_eq!(
        extract(&f, &l, &no_cancel()),
        Err(Error::LimitExceeded(Limit::XmpBytes))
    );
    // Standard and extended packet together.
    let f = jpeg(&[app1_xmp(&std), app1_ext(GUID_A, 10, 0, b"0123456789")]);
    let l = Limits {
        max_xmp_bytes: std.len() + 5,
        ..limits()
    };
    assert_eq!(
        extract(&f, &l, &no_cancel()),
        Err(Error::LimitExceeded(Limit::XmpBytes))
    );
    let l = Limits {
        max_xmp_bytes: std.len() + 10,
        ..limits()
    };
    assert!(
        extract(&f, &l, &no_cancel())
            .unwrap()
            .xmp_extended
            .is_some()
    );
}

#[test]
fn limit_max_alloc() {
    let t = tiff(false, &[short(false, 0x0112, 6)]);
    let f = jpeg(&[app1_exif(&t)]);
    let l = Limits {
        max_alloc: t.len() - 1,
        ..limits()
    };
    assert_eq!(
        extract(&f, &l, &no_cancel()),
        Err(Error::LimitExceeded(Limit::Alloc))
    );
    let std = std_xmp_attr(GUID_A);
    let f = jpeg(&[app1_xmp(&std), app1_ext(GUID_A, 1000, 0, b"0123456789")]);
    let l = Limits {
        max_alloc: std.len() + 100,
        ..limits()
    };
    assert_eq!(
        extract(&f, &l, &no_cancel()),
        Err(Error::LimitExceeded(Limit::Alloc))
    );
    let f = jpeg(&[app2_icc(1, 1, &[7; 100])]);
    let l = Limits {
        max_alloc: 99,
        ..limits()
    };
    assert_eq!(
        extract(&f, &l, &no_cancel()),
        Err(Error::LimitExceeded(Limit::Alloc))
    );
}

#[test]
fn cancel() {
    let t = tiff(false, &[short(false, 0x0112, 6)]);
    let f = jpeg(&[app1_exif(&t)]);
    let flag = AtomicBool::new(true);
    assert_eq!(extract(&f, &limits(), &flag), Err(Error::Cancelled));
    flag.store(false, Ordering::Relaxed);
    assert!(extract(&f, &limits(), &flag).is_ok());
}
