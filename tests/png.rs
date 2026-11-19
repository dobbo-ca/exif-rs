//! PNG: extract.

mod common;

use std::sync::atomic::AtomicBool;

use common::*;
use exif::{Container, Error, Limit, Limits, detect, extract};

fn t6() -> Vec<u8> {
    tiff(false, &[short(false, 0x0112, 6)])
}

fn raw_of(extra: &[Vec<u8>]) -> exif::Raw {
    extract(&png_with(extra), &limits(), &no_cancel()).unwrap()
}

#[test]
fn exif_chunk_with_and_without_prefix() {
    let t = t6();
    let raw = raw_of(&[chunk(b"eXIf", &t)]);
    assert_eq!(raw.container, Container::Png);
    assert_eq!(raw.exif, Some(t.clone()));
    let prefixed = [b"Exif\0\0".as_slice(), &t].concat();
    assert_eq!(raw_of(&[chunk(b"eXIf", &prefixed)]).exif, Some(t));
}

#[test]
fn xmp_itxt_plain_and_compressed() {
    let p = xmp_plain();
    for compressed in [false, true] {
        let raw = raw_of(&[itxt_xmp(p.as_bytes(), compressed)]);
        assert_eq!(raw.xmp.as_deref(), Some(p.as_bytes()), "{compressed}");
    }
}

#[test]
fn xmp_text_and_ztxt() {
    // As ExifTool: the XMP keyword in tEXt and zTXt too; the bytes as stored, UTF-8 or not.
    let p = [
        b"<?xpacket begin=\"\xef\xbb\xbf\xa9\"?>".as_slice(),
        xmp_plain().as_bytes(),
    ]
    .concat();
    let mut t = b"XML:com.adobe.xmp\0".to_vec();
    t.extend_from_slice(&p);
    let mut z = b"XML:com.adobe.xmp\0\0".to_vec();
    z.extend_from_slice(&zlib_stored(&p));
    for c in [chunk(b"tEXt", &t), chunk(b"zTXt", &z)] {
        assert_eq!(raw_of(&[c]).xmp, Some(p.clone()));
    }
    // The first XMP chunk wins.
    let raw = raw_of(&[chunk(b"tEXt", &t), itxt_xmp(b"<two/>", false)]);
    assert_eq!(raw.xmp, Some(p));
}

#[test]
fn raw_profiles_xmp_app1_xmp_and_icc() {
    let p = xmp_plain().into_bytes();
    let app1 = [b"http://ns.adobe.com/xap/1.0/\0".as_slice(), &p].concat();
    for c in [
        ztxt_profile("Raw profile type xmp", &p),
        text_profile("Raw profile type xmp", &p),
        text_profile("Raw profile type APP1", &app1),
    ] {
        let raw = raw_of(&[c]);
        assert_eq!(raw.xmp, Some(p.clone()));
        assert!(raw.exif.is_none());
    }
    for k in ["Raw profile type icc", "Raw profile type icm"] {
        let raw = raw_of(&[ztxt_profile(k, b"profile bytes")]);
        assert_eq!(raw.icc.as_deref(), Some(&b"profile bytes"[..]));
    }
    // iCCP wins over a raw profile, before or after it.
    for order in [false, true] {
        let mut cs = vec![ztxt_profile("Raw profile type icc", b"raw"), iccp(b"iccp")];
        if order {
            cs.reverse();
        }
        assert_eq!(raw_of(&cs).icc.as_deref(), Some(&b"iccp"[..]));
    }
    // A length that the hex text cannot hold is not a profile (and allocates nothing).
    let mut d = b"Raw profile type icc\0".to_vec();
    d.extend_from_slice(b"\nicc\n 999999999999\n00\n");
    assert!(raw_of(&[chunk(b"tEXt", &d)]).icc.is_none());
}

#[test]
fn other_itxt_is_not_xmp() {
    let mut d = b"Comment\0\0\0\0\0\0".to_vec();
    d.extend_from_slice(b"hello");
    assert!(raw_of(&[chunk(b"iTXt", &d)]).xmp.is_none());
}

#[test]
fn raw_profile_ztxt_text_and_itxt() {
    let t = t6();
    let block = [b"Exif\0\0".as_slice(), &t].concat();
    for c in [
        ztxt_profile("Raw profile type exif", &block),
        text_profile("Raw profile type exif", &block),
        text_profile("Raw profile type APP1", &block),
    ] {
        assert_eq!(raw_of(&[c]).exif, Some(t.clone()));
    }
    // iTXt, plain and compressed
    for compressed in [false, true] {
        let mut d = b"Raw profile type exif\0".to_vec();
        d.extend_from_slice(&[u8::from(compressed), 0, 0, 0]);
        let text = raw_profile("exif", &block);
        if compressed {
            d.extend_from_slice(&zlib_stored(&text));
        } else {
            d.extend_from_slice(&text);
        }
        assert_eq!(raw_of(&[chunk(b"iTXt", &d)]).exif, Some(t.clone()));
    }
}

#[test]
fn exif_chunk_wins_over_raw_profile() {
    let t1 = t6();
    let t2 = tiff(false, &[short(false, 0x0112, 3)]);
    let block = [b"Exif\0\0".as_slice(), &t1].concat();
    let raw = raw_of(&[
        text_profile("Raw profile type exif", &block),
        chunk(b"eXIf", &t2),
    ]);
    assert_eq!(raw.exif, Some(t2.clone()));
    let raw = raw_of(&[
        chunk(b"eXIf", &t2),
        text_profile("Raw profile type exif", &block),
    ]);
    assert_eq!(raw.exif, Some(t2));
}

#[test]
fn bad_raw_profile_is_ignored() {
    for text in [
        &b"\nexif\n      99\n0102\n"[..],
        b"\nexif\n       2\nzz\n",
        b"junk",
    ] {
        let mut d = b"Raw profile type exif\0".to_vec();
        d.extend_from_slice(text);
        assert!(raw_of(&[chunk(b"tEXt", &d)]).exif.is_none());
    }
}

#[test]
fn iccp_is_inflated() {
    let raw = raw_of(&[iccp(b"profile bytes")]);
    assert_eq!(raw.icc.as_deref(), Some(&b"profile bytes"[..]));
}

#[test]
fn corrupt_zlib_is_skipped() {
    let mut d = b"icc\0\0".to_vec();
    d.extend_from_slice(&[0xFF, 0xFF, 0xFF]);
    let raw = raw_of(&[chunk(b"iCCP", &d), chunk(b"eXIf", &t6())]);
    assert!(raw.icc.is_none());
    assert!(raw.exif.is_some());
}

#[test]
fn chunks_after_iend_are_ignored() {
    let mut f = png_with(&[]);
    f.extend_from_slice(&chunk(b"eXIf", &t6()));
    assert!(extract(&f, &limits(), &no_cancel()).unwrap().exif.is_none());
}

#[test]
fn not_a_png() {
    assert_eq!(detect(b"\x89PNG"), None);
    assert_eq!(
        extract(b"\x89PNG\r\n", &limits(), &no_cancel()),
        Err(Error::UnknownFormat)
    );
}

#[test]
fn truncated_at_every_length() {
    let t = t6();
    let f = png_with(&[
        chunk(b"eXIf", &t),
        itxt_xmp(xmp_plain().as_bytes(), true),
        iccp(b"profile"),
    ]);
    let exif_end = 8 + ihdr().len() + chunk(b"eXIf", &t).len();
    for n in 0..=f.len() {
        let r = extract(&f[..n], &limits(), &no_cancel());
        if n < 8 {
            assert_eq!(r, Err(Error::UnknownFormat), "n={n}");
        } else {
            assert_eq!(r.unwrap().exif.is_some(), n >= exif_end, "n={n}");
        }
    }
}

#[test]
fn limit_max_segments() {
    let f = png_with(&[]);
    let l = Limits {
        max_segments: 2,
        ..limits()
    };
    assert_eq!(
        extract(&f, &l, &no_cancel()),
        Err(Error::LimitExceeded(Limit::Segments))
    );
    let l = Limits {
        max_segments: 3,
        ..limits()
    };
    assert!(extract(&f, &l, &no_cancel()).is_ok());
}

#[test]
fn limit_max_exif_bytes() {
    let t = t6();
    let l = Limits {
        max_exif_bytes: t.len() - 1,
        ..limits()
    };
    let f = png_with(&[chunk(b"eXIf", &t)]);
    assert_eq!(
        extract(&f, &l, &no_cancel()),
        Err(Error::LimitExceeded(Limit::ExifBytes))
    );
    let block = [b"Exif\0\0".as_slice(), &t].concat();
    let f = png_with(&[ztxt_profile("Raw profile type exif", &block)]);
    assert_eq!(
        extract(&f, &l, &no_cancel()),
        Err(Error::LimitExceeded(Limit::ExifBytes))
    );
    let f = png_with(&[text_profile("Raw profile type exif", &block)]);
    assert_eq!(
        extract(&f, &l, &no_cancel()),
        Err(Error::LimitExceeded(Limit::ExifBytes))
    );
}

#[test]
fn limit_max_xmp_bytes_on_inflate_and_copy() {
    let p = vec![b' '; 100];
    let l = Limits {
        max_xmp_bytes: 10,
        ..limits()
    };
    for compressed in [false, true] {
        let f = png_with(&[itxt_xmp(&p, compressed)]);
        assert_eq!(
            extract(&f, &l, &no_cancel()),
            Err(Error::LimitExceeded(Limit::XmpBytes)),
            "{compressed}"
        );
    }
}

#[test]
fn limit_max_alloc() {
    let p = vec![b' '; 100];
    let l = Limits {
        max_alloc: 50,
        ..limits()
    };
    for f in [
        png_with(&[itxt_xmp(&p, true)]),
        png_with(&[itxt_xmp(&p, false)]),
        png_with(&[iccp(&p)]),
        png_with(&[chunk(b"eXIf", &p)]),
    ] {
        assert_eq!(
            extract(&f, &l, &no_cancel()),
            Err(Error::LimitExceeded(Limit::Alloc))
        );
    }
}

#[test]
fn cancel() {
    let f = png_with(&[iccp(b"profile")]);
    let flag = AtomicBool::new(true);
    assert_eq!(extract(&f, &limits(), &flag), Err(Error::Cancelled));
}
