//! WebP: extract.

mod common;

use std::sync::atomic::AtomicBool;

use common::*;
use exif::{Container, Error, Limit, Limits, detect, extract};

fn t6() -> Vec<u8> {
    tiff(false, &[short(false, 0x0112, 6)])
}

fn raw_of(extra: &[Vec<u8>]) -> exif::Raw {
    let mut chunks = vec![vp8x(0x2C, 16, 8), vp8()];
    chunks.extend_from_slice(extra);
    extract(&webp(&chunks), &limits(), &no_cancel()).unwrap()
}

#[test]
fn exif_with_and_without_prefix() {
    let t = t6();
    let raw = raw_of(&[riff_chunk(b"EXIF", &t)]);
    assert_eq!(raw.container, Container::Webp);
    assert_eq!(raw.exif.as_deref(), Some(t.as_slice()));
    let prefixed = [b"Exif\0\0".as_slice(), &t].concat();
    assert_eq!(raw_of(&[riff_chunk(b"EXIF", &prefixed)]).exif, Some(t));
}

#[test]
fn xmp_and_icc() {
    let p = xmp_plain();
    let raw = raw_of(&[
        riff_chunk(b"XMP ", p.as_bytes()),
        riff_chunk(b"ICCP", b"icc"),
    ]);
    assert_eq!(raw.xmp.as_deref(), Some(p.as_bytes()));
    assert_eq!(raw.icc.as_deref(), Some(&b"icc"[..]));
    assert!(raw.xmp_extended.is_none());
}

#[test]
fn riff_size_is_not_trusted() {
    let t = t6();
    let mut f = webp(&[vp8x(0x08, 16, 8), vp8(), riff_chunk(b"EXIF", &t)]);
    // larger than the data
    f[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(extract(&f, &limits(), &no_cancel()).unwrap().exif, Some(t));
    // smaller: the chunks end there
    let end = (12 + vp8x(0, 16, 8).len() + vp8().len()) as u32 - 8;
    f[4..8].copy_from_slice(&end.to_le_bytes());
    assert!(extract(&f, &limits(), &no_cancel()).unwrap().exif.is_none());
}

#[test]
fn not_a_webp() {
    assert_eq!(detect(b"RIFF\0\0\0\0WAVE"), None);
    assert_eq!(
        extract(b"RIFF\0\0\0\0WAVEfmt ", &limits(), &no_cancel()),
        Err(Error::UnknownFormat)
    );
}

#[test]
fn truncated_at_every_length() {
    let t = t6();
    let f = webp(&[
        vp8x(0x2C, 16, 8),
        vp8(),
        riff_chunk(b"EXIF", &t),
        riff_chunk(b"XMP ", xmp_plain().as_bytes()),
    ]);
    let exif_end = 12 + vp8x(0, 1, 1).len() + vp8().len() + 8 + t.len();
    for n in 0..=f.len() {
        let r = extract(&f[..n], &limits(), &no_cancel());
        if n < 12 {
            assert_eq!(r, Err(Error::UnknownFormat), "n={n}");
        } else {
            assert_eq!(r.unwrap().exif.is_some(), n >= exif_end, "n={n}");
        }
    }
}

#[test]
fn limit_max_segments() {
    let f = webp(&[vp8x(0, 16, 8), vp8(), vp8()]);
    let l = Limits {
        max_segments: 2,
        ..limits()
    };
    assert_eq!(
        extract(&f, &l, &no_cancel()),
        Err(Error::LimitExceeded(Limit::Segments))
    );
    let l = Limits {
        max_segments: 4,
        ..limits()
    };
    assert!(extract(&f, &l, &no_cancel()).is_ok());
}

#[test]
fn limit_max_exif_and_xmp_bytes() {
    let t = t6();
    let f = webp(&[vp8x(0x0C, 16, 8), vp8(), riff_chunk(b"EXIF", &t)]);
    let l = Limits {
        max_exif_bytes: t.len() - 1,
        ..limits()
    };
    assert_eq!(
        extract(&f, &l, &no_cancel()),
        Err(Error::LimitExceeded(Limit::ExifBytes))
    );
    let f = webp(&[vp8x(0x0C, 16, 8), vp8(), riff_chunk(b"XMP ", &[b' '; 20])]);
    let l = Limits {
        max_xmp_bytes: 19,
        ..limits()
    };
    assert_eq!(
        extract(&f, &l, &no_cancel()),
        Err(Error::LimitExceeded(Limit::XmpBytes))
    );
}

#[test]
fn limit_max_alloc() {
    let f = webp(&[vp8x(0x2C, 16, 8), vp8(), riff_chunk(b"ICCP", &[1; 100])]);
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
    let f = webp(&[vp8x(0, 16, 8), vp8()]);
    let flag = AtomicBool::new(true);
    assert_eq!(extract(&f, &limits(), &flag), Err(Error::Cancelled));
}
