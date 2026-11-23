//! strip and reset_orientation, for JPEG, PNG and WebP.

mod common;

use std::sync::atomic::AtomicBool;

use common::*;
use exif::{Error, Keep, Limit, Limits, extract, read, reset_orientation, strip};

const NONE: Keep = Keep::NONE;
const ORIENTATION: Keep = Keep {
    orientation: true,
    ..Keep::NONE
};
const ICC: Keep = Keep {
    icc: true,
    ..Keep::NONE
};
const BOTH: Keep = Keep {
    orientation: true,
    icc: true,
    ..Keep::NONE
};

fn run_strip(f: &[u8], keep: Keep) -> Vec<u8> {
    strip(f, keep, &limits(), &no_cancel()).unwrap()
}

fn run_reset(f: &[u8]) -> Vec<u8> {
    reset_orientation(f, &limits(), &no_cancel()).unwrap()
}

/// The block `strip` writes for an orientation value.
fn min_exif(be: bool, v: u16) -> Vec<u8> {
    tiff(be, &[short(be, 0x0112, v)])
}

/// `f` with the orientation value of the TIFF block `t` (which starts with the orientation
/// entry) set to 1.
fn with_orientation_one(f: &[u8], t: &[u8], be: bool) -> Vec<u8> {
    let at = f.windows(t.len()).position(|w| w == t).unwrap() + ORIENTATION_VALUE_AT;
    let mut out = f.to_vec();
    out[at..at + 2].copy_from_slice(&if be { [0, 1] } else { [1, 0] });
    out
}

fn orientation_of(f: &[u8]) -> Option<u16> {
    read(f, &limits(), &no_cancel())
        .unwrap()
        .exif
        .unwrap()
        .orientation()
}

// ---- JPEG strip ----

fn adobe() -> Vec<u8> {
    seg(0xEE, b"Adobe\0\0\0\0\0\0\0")
}

fn full_jpeg(be: bool) -> Vec<u8> {
    let t = tiff(be, &[ascii(0x010F, "Canon"), short(be, 0x0112, 3)]);
    jpeg(&[
        app0_jfif(),
        app1_exif(&t),
        app1_xmp(xmp_plain().as_bytes()),
        app2_icc(1, 2, b"ab"),
        app2_icc(2, 2, b"cd"),
        seg(0xFE, b"comment"),
        adobe(),
        seg(0xE5, b"junk"),
        dqt(),
    ])
}

#[test]
fn jpeg_strip_variants() {
    for be in [false, true] {
        let f = full_jpeg(be);
        let min = app1_exif(&min_exif(be, 3));
        let cases = [
            (NONE, jpeg(&[app0_jfif(), adobe(), dqt()])),
            (
                ORIENTATION,
                jpeg(&[app0_jfif(), min.clone(), adobe(), dqt()]),
            ),
            (
                ICC,
                jpeg(&[
                    app0_jfif(),
                    app2_icc(1, 2, b"ab"),
                    app2_icc(2, 2, b"cd"),
                    adobe(),
                    dqt(),
                ]),
            ),
            (
                BOTH,
                jpeg(&[
                    app0_jfif(),
                    min,
                    app2_icc(1, 2, b"ab"),
                    app2_icc(2, 2, b"cd"),
                    adobe(),
                    dqt(),
                ]),
            ),
        ];
        for (keep, want) in cases {
            let out = run_strip(&f, keep);
            assert_eq!(out, want, "be={be} {keep:?}");
            assert_eq!(out[sos_at(&out)..], f[sos_at(&f)..]);
            let raw = extract(&out, &limits(), &no_cancel()).unwrap();
            assert!(raw.xmp.is_none());
            assert_eq!(raw.exif.is_some(), keep.orientation);
            assert_eq!(raw.icc.as_deref(), keep.icc.then_some(&b"abcd"[..]));
        }
        assert_eq!(orientation_of(&run_strip(&f, ORIENTATION)), Some(3));
    }
}

#[test]
fn jpeg_exif_goes_first_without_jfif() {
    let t = min_exif(false, 8);
    let f = jpeg(&[app1_exif(&t), dqt()]);
    assert_eq!(run_strip(&f, ORIENTATION), jpeg(&[app1_exif(&t), dqt()]));
    let f = jpeg(&[dqt(), app1_xmp(b"<x/>"), app1_exif(&t)]);
    assert_eq!(run_strip(&f, ORIENTATION), jpeg(&[app1_exif(&t), dqt()]));
}

#[test]
fn jpeg_wrong_tiff_magic_is_read_and_kept() {
    // "MM" then 0x132A instead of 42: ExifTool only warns, and so reads the block.
    let mut t = min_exif(true, 6);
    t[2] = 0x13;
    let f = jpeg(&[app0_jfif(), app1_exif(&t), dqt()]);
    assert_eq!(orientation_of(&f), Some(6));
    assert_eq!(
        run_strip(&f, ORIENTATION),
        jpeg(&[app0_jfif(), app1_exif(&min_exif(true, 6)), dqt()])
    );
}

#[test]
fn jpeg_no_orientation_keeps_nothing() {
    let t = tiff(false, &[ascii(0x010F, "Canon")]);
    let f = jpeg(&[app1_exif(&t), dqt()]);
    assert_eq!(run_strip(&f, BOTH), jpeg(&[dqt()]));
}

#[test]
fn jpeg_soi_eoi_only() {
    let f = [0xFF, 0xD8, 0xFF, 0xD9];
    for keep in [NONE, ORIENTATION, ICC, BOTH] {
        assert_eq!(run_strip(&f, keep), f);
    }
}

#[test]
fn jpeg_scan_bytes_and_trailer() {
    let hdr = [0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00];
    let mut tail = hdr.to_vec();
    tail.extend_from_slice(&[0x12, 0x34, 0xFF, 0x00]);
    // a segment between scans that holds FF D9 in its payload
    tail.extend_from_slice(&seg(0xDB, &[0, 0xFF, 0xD9, 0]));
    tail.extend_from_slice(&hdr);
    tail.extend_from_slice(&[0x56, 0x78, 0xFF, 0xD0, 0x9A, 0xFF, 0xD9]);
    let mut f = vec![0xFF, 0xD8];
    f.extend_from_slice(&app1_xmp(b"<x/>"));
    f.extend_from_slice(&tail);
    let mut want = vec![0xFF, 0xD8];
    want.extend_from_slice(&tail);
    f.extend_from_slice(b"trailer after EOI");
    assert_eq!(run_strip(&f, NONE), want);
}

#[test]
fn jpeg_no_eoi_copies_to_the_end() {
    let mut f = jpeg(&[app1_xmp(b"<x/>"), dqt()]);
    f.truncate(f.len() - 5);
    let out = run_strip(&f, NONE);
    assert_eq!(out[..2], [0xFF, 0xD8]);
    assert_eq!(out[2..], f[sos_at(&f) - dqt().len()..]);
}

#[test]
fn jpeg_truncated_header_is_truncated() {
    let t = min_exif(false, 6);
    let f = jpeg(&[app0_jfif(), app1_exif(&t), dqt()]);
    for n in 3..sos_at(&f) {
        for keep in [NONE, BOTH] {
            assert_eq!(
                strip(&f[..n], keep, &limits(), &no_cancel()),
                Err(Error::Truncated),
                "n={n}"
            );
        }
    }
    for n in 0..=f.len() {
        let _ = strip(&f[..n], BOTH, &limits(), &no_cancel());
        let _ = reset_orientation(&f[..n], &limits(), &no_cancel());
    }
}

// ---- PNG strip ----

fn full_png(be: bool) -> Vec<u8> {
    let t = tiff(be, &[ascii(0x010F, "Canon"), short(be, 0x0112, 6)]);
    png(&[
        ihdr(),
        chunk(b"gAMA", &[0, 0, 0, 1]),
        chunk(b"eXIf", &t),
        itxt_xmp(xmp_plain().as_bytes(), false),
        chunk(b"tEXt", b"Comment\0hi"),
        iccp(b"profile"),
        chunk(b"pHYs", &[0; 9]),
        idat(),
        iend(),
    ])
}

#[test]
fn png_strip_variants() {
    for be in [false, true] {
        let f = full_png(be);
        let min = chunk(b"eXIf", &min_exif(be, 6));
        let g = chunk(b"gAMA", &[0, 0, 0, 1]);
        let p = chunk(b"pHYs", &[0; 9]);
        let cases = [
            (NONE, png(&[ihdr(), g.clone(), p.clone(), idat(), iend()])),
            (
                ORIENTATION,
                png(&[ihdr(), g.clone(), p.clone(), min.clone(), idat(), iend()]),
            ),
            (
                ICC,
                png(&[
                    ihdr(),
                    g.clone(),
                    iccp(b"profile"),
                    p.clone(),
                    idat(),
                    iend(),
                ]),
            ),
            (
                BOTH,
                png(&[ihdr(), g, iccp(b"profile"), p, min, idat(), iend()]),
            ),
        ];
        for (keep, want) in cases {
            let out = run_strip(&f, keep);
            assert_eq!(out, want, "be={be} {keep:?}");
            assert!(png_chunks(&out).iter().all(|c| c.2));
            let raw = extract(&out, &limits(), &no_cancel()).unwrap();
            assert!(raw.xmp.is_none());
            assert_eq!(raw.exif.is_some(), keep.orientation);
            assert_eq!(raw.icc.is_some(), keep.icc);
        }
    }
}

#[test]
fn png_orientation_from_a_raw_profile() {
    let t = min_exif(false, 5);
    let block = [b"Exif\0\0".as_slice(), &t].concat();
    let f = png_with(&[text_profile("Raw profile type exif", &block)]);
    assert_eq!(run_strip(&f, ORIENTATION), png_with(&[chunk(b"eXIf", &t)]));
}

#[test]
fn png_truncated_is_truncated() {
    let f = full_png(false);
    for n in 8..f.len() {
        assert_eq!(
            strip(&f[..n], ORIENTATION, &limits(), &no_cancel()),
            Err(Error::Truncated),
            "n={n}"
        );
    }
    for n in 0..=f.len() {
        let _ = reset_orientation(&f[..n], &limits(), &no_cancel());
    }
}

#[test]
fn png_unknown_chunks_follow_keep_unknown() {
    let t = tiff(false, &[ascii(0x010F, "Canon"), short(false, 0x0112, 6)]);
    let icc = iccp(b"profile");
    let private = chunk(b"prVt", b"private data");
    let cabx = chunk(b"caBX", b"jumbf");
    let late = chunk(b"gmAP", b"after IDAT");
    let f = png(&[
        ihdr(),
        icc.clone(),
        private.clone(),
        chunk(b"eXIf", &t),
        itxt_xmp(xmp_plain().as_bytes(), false),
        chunk(b"tEXt", b"Comment\0hi"),
        ztxt_profile("Raw profile type xmp", b"<x/>"),
        chunk(b"tIME", &[0x07, 0xEA, 11, 23, 12, 0, 0]),
        chunk(b"zxIf", &t),
        cabx.clone(),
        idat(),
        late.clone(),
        iend(),
    ]);
    let min = chunk(b"eXIf", &min_exif(false, 6));
    let no_unknown = Keep {
        unknown: false,
        ..Keep::default()
    };
    let cases = [
        (
            Keep::default(),
            png(&[
                ihdr(),
                icc.clone(),
                private,
                cabx,
                min.clone(),
                idat(),
                late,
                iend(),
            ]),
        ),
        (no_unknown, png(&[ihdr(), icc, min, idat(), iend()])),
        (NONE, png(&[ihdr(), idat(), iend()])),
    ];
    for (keep, want) in cases {
        let out = run_strip(&f, keep);
        assert_eq!(out, want, "{keep:?}");
        assert!(png_chunks(&out).iter().all(|c| c.2));
        let raw = extract(&out, &limits(), &no_cancel()).unwrap();
        assert!(raw.xmp.is_none());
        assert_eq!(raw.exif.is_some(), keep.orientation);
    }
}

#[test]
fn png_bytes_after_iend_follow_keep_trailing() {
    let t = tiff(false, &[ascii(0x010F, "Canon"), short(false, 0x0112, 6)]);
    let tail = [chunk(b"eXIf", &t), b"TRAILER".to_vec()].concat();
    let mut f = png_with(&[chunk(b"eXIf", &t)]);
    f.extend_from_slice(&tail);
    let body = png_with(&[chunk(b"eXIf", &min_exif(false, 6))]);
    let no_trailing = Keep {
        trailing: false,
        ..Keep::default()
    };
    let cases = [
        (Keep::default(), [body.as_slice(), &tail].concat()),
        (no_trailing, body),
        (NONE, png_with(&[])),
    ];
    for (keep, want) in cases {
        assert_eq!(run_strip(&f, keep), want, "{keep:?}");
    }
    // reset_orientation keeps them too.
    assert!(run_reset(&f).ends_with(&tail));
}

#[test]
fn png_new_exif_goes_before_idot() {
    let t = tiff(false, &[short(false, 0x0112, 6)]);
    // Apple's iDOT: the offset of the first IDAT counts from the iDOT chunk.
    let idot = chunk(
        b"iDOT",
        &[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0x18],
    );
    let f = png(&[ihdr(), chunk(b"eXIf", &t), idot.clone(), idat(), iend()]);
    let min = chunk(b"eXIf", &min_exif(false, 6));
    // The IDAT still starts right after the iDOT chunk.
    assert_eq!(
        run_strip(&f, Keep::default()),
        png(&[ihdr(), min, idot, idat(), iend()])
    );
}

// ---- WebP strip ----

fn full_webp(be: bool) -> Vec<u8> {
    let t = tiff(be, &[ascii(0x010F, "Canon"), short(be, 0x0112, 2)]);
    webp(&[
        vp8x(0x3C, 16, 8),
        riff_chunk(b"ICCP", b"icc"),
        vp8(),
        riff_chunk(b"EXIF", &[b"Exif\0\0".as_slice(), &t].concat()),
        riff_chunk(b"XMP ", xmp_plain().as_bytes()),
        riff_chunk(b"abcd", b"unknown"),
    ])
}

#[test]
fn webp_strip_variants() {
    for be in [false, true] {
        let f = full_webp(be);
        let min = riff_chunk(b"EXIF", &min_exif(be, 2));
        let icc = riff_chunk(b"ICCP", b"icc");
        let cases = [
            (NONE, webp(&[vp8x(0x10, 16, 8), vp8()])),
            (ORIENTATION, webp(&[vp8x(0x18, 16, 8), vp8(), min.clone()])),
            (ICC, webp(&[vp8x(0x30, 16, 8), icc.clone(), vp8()])),
            (BOTH, webp(&[vp8x(0x38, 16, 8), icc, vp8(), min])),
        ];
        for (keep, want) in cases {
            let out = run_strip(&f, keep);
            assert_eq!(out, want, "be={be} {keep:?}");
            webp_chunks(&out);
            let raw = extract(&out, &limits(), &no_cancel()).unwrap();
            assert!(raw.xmp.is_none());
            assert_eq!(raw.exif.is_some(), keep.orientation);
            assert_eq!(raw.icc.is_some(), keep.icc);
        }
    }
}

#[test]
fn webp_simple_file_gets_a_vp8x_for_the_orientation() {
    let t = min_exif(false, 6);
    let exif = riff_chunk(b"EXIF", &t);
    for image in [vp8(), vp8l()] {
        let f = webp(&[image.clone(), exif.clone()]);
        assert_eq!(
            run_strip(&f, ORIENTATION),
            webp(&[vp8x(0x08, 16, 8), image.clone(), exif.clone()])
        );
        assert_eq!(run_strip(&f, NONE), webp(&[image]));
    }
}

#[test]
fn webp_cut_is_never_a_panic() {
    let f = full_webp(false);
    for n in 0..=f.len() {
        let r = strip(&f[..n], BOTH, &limits(), &no_cancel());
        assert!(
            matches!(r, Ok(_) | Err(Error::Truncated | Error::UnknownFormat)),
            "n={n}"
        );
        let _ = reset_orientation(&f[..n], &limits(), &no_cancel());
    }
}

#[test]
fn webp_unknown_chunks_follow_keep_unknown() {
    let t = tiff(false, &[ascii(0x010F, "Canon"), short(false, 0x0112, 3)]);
    let icc = riff_chunk(b"ICCP", b"icc");
    // Odd length: stored with its pad byte.
    let private = riff_chunk(b"prVt", b"odd");
    let c2pa = riff_chunk(b"C2PA", b"jumbf!");
    let f = webp(&[
        vp8x(0x2C, 16, 8),
        icc.clone(),
        private.clone(),
        vp8(),
        riff_chunk(b"EXIF", &t),
        riff_chunk(b"XMP ", xmp_plain().as_bytes()),
        riff_chunk(b"XMP\0", b"<x/>"),
        c2pa.clone(),
    ]);
    let min = riff_chunk(b"EXIF", &min_exif(false, 3));
    let no_unknown = Keep {
        unknown: false,
        ..Keep::default()
    };
    let cases = [
        (
            Keep::default(),
            webp(&[
                vp8x(0x28, 16, 8),
                icc.clone(),
                private,
                vp8(),
                c2pa,
                min.clone(),
            ]),
        ),
        (no_unknown, webp(&[vp8x(0x28, 16, 8), icc, vp8(), min])),
        (NONE, webp(&[vp8x(0x00, 16, 8), vp8()])),
    ];
    for (keep, want) in cases {
        let out = run_strip(&f, keep);
        assert_eq!(out, want, "{keep:?}");
        webp_chunks(&out);
        let raw = extract(&out, &limits(), &no_cancel()).unwrap();
        assert!(raw.xmp.is_none());
        assert_eq!(raw.exif.is_some(), keep.orientation);
    }
}

#[test]
fn webp_added_vp8x_goes_before_kept_chunks() {
    let private = riff_chunk(b"prVt", b"odd");
    let exif = riff_chunk(b"EXIF", &min_exif(false, 6));
    let f = webp(&[private.clone(), vp8(), exif.clone()]);
    assert_eq!(
        run_strip(&f, Keep::default()),
        webp(&[vp8x(0x08, 16, 8), private, vp8(), exif])
    );
}

#[test]
fn webp_unknown_chunk_without_its_pad_byte_gets_one() {
    let mut f = webp(&[vp8x(0x00, 16, 8), vp8(), riff_chunk(b"prVt", b"odd")]);
    f.pop();
    let riff = u32::try_from(f.len() - 8).unwrap();
    f[4..8].copy_from_slice(&riff.to_le_bytes());
    let want = webp(&[vp8x(0x00, 16, 8), vp8(), riff_chunk(b"prVt", b"odd")]);
    let out = run_strip(&f, Keep::default());
    assert_eq!(out, want);
    webp_chunks(&out);
}

#[test]
fn webp_bytes_after_the_riff_size_follow_keep_trailing() {
    let t = tiff(false, &[ascii(0x010F, "Canon"), short(false, 0x0112, 6)]);
    let tail = [riff_chunk(b"EXIF", &t), b"TRAILER!".to_vec()].concat();
    let f = [
        webp(&[vp8x(0x08, 16, 8), vp8(), riff_chunk(b"EXIF", &t)]),
        tail.clone(),
    ]
    .concat();
    let body = webp(&[
        vp8x(0x08, 16, 8),
        vp8(),
        riff_chunk(b"EXIF", &min_exif(false, 6)),
    ]);
    let no_trailing = Keep {
        trailing: false,
        ..Keep::default()
    };
    // ExifTool reads the EXIF chunk there: it goes; the bytes that are not a chunk stay.
    let cases = [
        (Keep::default(), [body.as_slice(), b"TRAILER!"].concat()),
        (no_trailing, body),
        (NONE, webp(&[vp8x(0x00, 16, 8), vp8()])),
    ];
    for (keep, want) in cases {
        assert_eq!(run_strip(&f, keep), want, "{keep:?}");
    }
    assert!(run_reset(&f).ends_with(&tail));

    // A RIFF size without the last pad byte: the pad byte is not part of the trailing bytes.
    let mut g = webp(&[vp8(), riff_chunk(b"prVt", b"odd")]);
    let riff = u32::try_from(g.len() - 8 - 1).unwrap();
    g[4..8].copy_from_slice(&riff.to_le_bytes());
    g.extend_from_slice(b"TAIL");
    let want = [
        webp(&[vp8(), riff_chunk(b"prVt", b"odd")]),
        b"TAIL".to_vec(),
    ]
    .concat();
    assert_eq!(run_strip(&g, Keep::default()), want);
}

#[test]
fn webp_metadata_past_a_short_riff_size_is_dropped() {
    let t = tiff(false, &[ascii(0x010F, "Canon"), short(false, 0x0112, 6)]);
    let private = riff_chunk(b"prVt", b"odd");
    let f = webp(&[
        vp8x(0x0C, 16, 8),
        vp8(),
        riff_chunk(b"EXIF", &t),
        riff_chunk(b"XMP ", xmp_plain().as_bytes()),
        riff_chunk(b"XMP\0", b"<x/>"),
        private.clone(),
    ]);
    // The RIFF size ends before the EXIF chunk: the orientation is not seen, the metadata
    // chunks after it still go.
    let head = webp(&[vp8x(0x0C, 16, 8), vp8()]);
    let mut g = f.clone();
    let riff = u32::try_from(head.len() - 8).unwrap();
    g[4..8].copy_from_slice(&riff.to_le_bytes());
    let want = [webp(&[vp8x(0x00, 16, 8), vp8()]), private.clone()].concat();
    assert_eq!(run_strip(&g, Keep::default()), want);
    // A RIFF size under 4: every chunk is after it.
    for riff in [0u32, 4] {
        let mut g = f.clone();
        g[4..8].copy_from_slice(&riff.to_le_bytes());
        let want = [
            b"RIFF\x04\0\0\0WEBP".as_slice(),
            &vp8x(0x0C, 16, 8),
            &vp8(),
            &private,
        ]
        .concat();
        assert_eq!(run_strip(&g, Keep::default()), want, "riff={riff}");
    }
}

#[test]
fn webp_second_or_short_vp8x_is_always_dropped() {
    let f = webp(&[
        vp8x(0x00, 16, 8),
        vp8x(0x00, 16, 8),
        riff_chunk(b"VP8X", b"short"),
        vp8(),
    ]);
    assert_eq!(
        run_strip(&f, Keep::default()),
        webp(&[vp8x(0x00, 16, 8), vp8()])
    );
}

// ---- reset_orientation ----

#[test]
fn jpeg_reset_exif_in_place() {
    for be in [false, true] {
        let t = min_exif(be, 6);
        let f = jpeg(&[app0_jfif(), app1_exif(&t), dqt()]);
        let out = run_reset(&f);
        assert_eq!(out, with_orientation_one(&f, &t, be));
        assert_eq!(out.len(), f.len());
        assert_eq!(orientation_of(&out), Some(1));
    }
}

#[test]
fn jpeg_reset_xmp_value() {
    for (orig, new) in [("6", "1"), ("06", "1")] {
        let f = jpeg(&[app1_xmp(xmp_attr(orig).as_bytes()), dqt()]);
        assert_eq!(
            run_reset(&f),
            jpeg(&[app1_xmp(xmp_attr(new).as_bytes()), dqt()])
        );
    }
}

#[test]
fn jpeg_reset_both_in_either_order() {
    let t = min_exif(false, 8);
    let x = xmp_attr("08");
    let want_exif = app1_exif(&min_exif(false, 1));
    let want_xmp = app1_xmp(xmp_attr("1").as_bytes());
    let f = jpeg(&[app1_exif(&t), app1_xmp(x.as_bytes())]);
    assert_eq!(run_reset(&f), jpeg(&[want_exif.clone(), want_xmp.clone()]));
    let f = jpeg(&[app1_xmp(x.as_bytes()), app1_exif(&t)]);
    assert_eq!(run_reset(&f), jpeg(&[want_xmp, want_exif]));
}

#[test]
fn reset_without_orientation_is_identity() {
    let t = tiff(false, &[ascii(0x010F, "Canon")]);
    let x = xmp_plain();
    let files = [
        jpeg(&[app1_exif(&t), app1_xmp(x.as_bytes()), dqt()]),
        png_with(&[chunk(b"eXIf", &t), itxt_xmp(x.as_bytes(), false)]),
        webp(&[
            vp8x(0x0C, 16, 8),
            vp8(),
            riff_chunk(b"EXIF", &t),
            riff_chunk(b"XMP ", x.as_bytes()),
        ]),
        jpeg(&[dqt()]),
        png_with(&[]),
        webp(&[vp8()]),
        vec![0xFF, 0xD8, 0xFF, 0xD9],
    ];
    for f in files {
        assert_eq!(run_reset(&f), f);
    }
}

#[test]
fn png_reset_exif_and_xmp_keep_valid_crcs() {
    for be in [false, true] {
        let t = min_exif(be, 6);
        let mut prefixed = b"Exif\0\0".to_vec();
        prefixed.extend_from_slice(&t);
        for data in [t.clone(), prefixed] {
            let f = png_with(&[chunk(b"eXIf", &data)]);
            let out = run_reset(&f);
            let one = min_exif(be, 1);
            let want_data = if data.len() > t.len() {
                [b"Exif\0\0".as_slice(), &one].concat()
            } else {
                one
            };
            assert_eq!(out, png_with(&[chunk(b"eXIf", &want_data)]));
            assert!(png_chunks(&out).iter().all(|c| c.2));
        }
    }
    for (orig, new) in [("6", "1"), ("06", "1")] {
        let f = png_with(&[
            itxt_xmp(xmp_attr(orig).as_bytes(), false),
            chunk(b"eXIf", &min_exif(false, 3)),
        ]);
        let out = run_reset(&f);
        assert_eq!(
            out,
            png_with(&[
                itxt_xmp(xmp_attr(new).as_bytes(), false),
                chunk(b"eXIf", &min_exif(false, 1)),
            ])
        );
        assert!(png_chunks(&out).iter().all(|c| c.2));
    }
}

#[test]
fn png_reset_xmp_in_text() {
    let text = |o: &str| {
        chunk(
            b"tEXt",
            format!("XML:com.adobe.xmp\0{}", xmp_attr(o)).as_bytes(),
        )
    };
    let out = run_reset(&png_with(&[text("06")]));
    assert_eq!(out, png_with(&[text("1")]));
    assert!(png_chunks(&out).iter().all(|c| c.2));
}

#[test]
fn png_keep_icc_keeps_a_raw_icc_profile() {
    let icc = ztxt_profile("Raw profile type icc", b"profile");
    let f = png_with(&[icc.clone(), ztxt_profile("Raw profile type xmp", b"<x/>")]);
    assert_eq!(run_strip(&f, ICC), png_with(&[icc]));
    assert_eq!(run_strip(&f, NONE), png_with(&[]));
    let raw = extract(&run_strip(&f, ICC), &limits(), &no_cancel()).unwrap();
    assert_eq!(raw.icc.as_deref(), Some(&b"profile"[..]));
}

#[test]
fn png_reset_leaves_compressed_xmp_unchanged() {
    let f = png_with(&[itxt_xmp(xmp_attr("6").as_bytes(), true)]);
    assert_eq!(run_reset(&f), f);
}

#[test]
fn webp_reset_exif_and_xmp() {
    for be in [false, true] {
        let t = min_exif(be, 6);
        let f = webp(&[vp8x(0x0C, 16, 8), vp8(), riff_chunk(b"EXIF", &t)]);
        assert_eq!(
            run_reset(&f),
            webp(&[
                vp8x(0x0C, 16, 8),
                vp8(),
                riff_chunk(b"EXIF", &min_exif(be, 1))
            ])
        );
    }
    // the XMP chunk gets one byte shorter: size, pad and RIFF size follow; a chunk after it moves
    for (orig, new) in [("06", "1"), ("006", "1")] {
        let f = webp(&[
            vp8x(0x0C, 16, 8),
            vp8(),
            riff_chunk(b"XMP ", xmp_attr(orig).as_bytes()),
            riff_chunk(b"EXIF", &min_exif(false, 5)),
        ]);
        let out = run_reset(&f);
        assert_eq!(
            out,
            webp(&[
                vp8x(0x0C, 16, 8),
                vp8(),
                riff_chunk(b"XMP ", xmp_attr(new).as_bytes()),
                riff_chunk(b"EXIF", &min_exif(false, 1)),
            ])
        );
        webp_chunks(&out);
    }
}

#[test]
fn orientation_of_any_integer_type_is_kept_and_reset() {
    // IFD0 Orientation stored as LONG (ExifTool reads any integer type), LE and BE; and an
    // SSHORT stored out of line with count 3.
    for be in [false, true] {
        let long = Ent {
            tag: 0x0112,
            typ: 4,
            count: 1,
            data: (if be {
                6u32.to_be_bytes()
            } else {
                6u32.to_le_bytes()
            })
            .to_vec(),
        };
        let t = tiff(be, &[long]);
        let f = jpeg(&[app1_exif(&t), dqt()]);
        assert_eq!(orientation_of(&f), Some(6));
        assert_eq!(
            run_strip(&f, ORIENTATION),
            jpeg(&[app1_exif(&min_exif(be, 6)), dqt()])
        );
        let out = run_reset(&f);
        assert_eq!(orientation_of(&out), Some(1));
        let mut want = f.clone();
        let at = f.windows(t.len()).position(|w| w == t).unwrap() + ORIENTATION_VALUE_AT;
        want[at..at + 4].copy_from_slice(&if be { [0, 0, 0, 1] } else { [1, 0, 0, 0] });
        assert_eq!(out, want);
    }
    let sshort = Ent {
        tag: 0x0112,
        typ: 8,
        count: 3,
        data: [8u16, 7, 6].iter().flat_map(|v| v.to_le_bytes()).collect(),
    };
    let t = tiff(false, &[sshort]);
    let f = png_with(&[chunk(b"eXIf", &t)]);
    assert_eq!(orientation_of(&f), Some(8));
    let out = run_reset(&f);
    assert_eq!(orientation_of(&out), Some(1));
    assert!(png_chunks(&out).iter().all(|c| c.2));
}

#[test]
fn png_reset_without_a_change_keeps_a_wrong_crc() {
    // No orientation in the eXIf chunk, a wrong stored CRC: the output equals the input.
    let mut c = chunk(b"eXIf", &tiff(false, &[ascii(0x010F, "Canon")]));
    let n = c.len();
    c[n - 1] ^= 0xFF;
    let f = png_with(&[c]);
    assert_eq!(run_reset(&f), f);
    // Orientation already 1 in EXIF and XMP: unchanged too.
    let mut c = chunk(b"eXIf", &min_exif(false, 1));
    let n = c.len();
    c[n - 1] ^= 0xFF;
    let mut x = itxt_xmp(xmp_attr("1").as_bytes(), false);
    let n = x.len();
    x[n - 1] ^= 0xFF;
    let f = png_with(&[c, x]);
    assert_eq!(run_reset(&f), f);
}

#[test]
fn reset_xmp_scan_follows_the_call_limits_and_cancel() {
    // tiff:Orientation after 10 property elements: a node limit of 5 is an error, as in read(),
    // not a silent half reset.
    let mut body = String::new();
    for i in 0..10 {
        body.push_str(&format!("<dc:p{i}>v</dc:p{i}>"));
    }
    let x = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:tiff=\"http://ns.adobe.com/tiff/1.0/\">{body}<tiff:Orientation>6</tiff:Orientation></rdf:Description></rdf:RDF></x:xmpmeta>"
    );
    let files = [
        png_with(&[
            chunk(b"eXIf", &min_exif(false, 6)),
            itxt_xmp(x.as_bytes(), false),
        ]),
        jpeg(&[
            app1_exif(&min_exif(false, 6)),
            app1_xmp(x.as_bytes()),
            dqt(),
        ]),
        webp(&[vp8(), riff_chunk(b"XMP ", x.as_bytes())]),
    ];
    for f in &files {
        let l = Limits {
            max_xmp_nodes: 5,
            ..limits()
        };
        assert_eq!(
            reset_orientation(f, &l, &no_cancel()),
            Err(Error::LimitExceeded(Limit::XmpNodes))
        );
        let l = Limits {
            max_xmp_bytes: x.len() - 1,
            ..limits()
        };
        assert_eq!(
            reset_orientation(f, &l, &no_cancel()),
            Err(Error::LimitExceeded(Limit::XmpBytes))
        );
        let out = run_reset(f);
        let needle = b"<tiff:Orientation>1</tiff:Orientation>";
        assert!(out.windows(needle.len()).any(|w| w == needle));
    }
}

#[test]
fn reset_changes_the_top_level_xmp_orientation_only() {
    // A Photoshop Pantry item's tiff:Orientation is a struct field; the top-level value is reset.
    let x = |o: &str| {
        format!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:xmpMM=\"http://ns.adobe.com/xap/1.0/mm/\" xmlns:tiff=\"http://ns.adobe.com/tiff/1.0/\"><xmpMM:Pantry><rdf:Bag><rdf:li><rdf:Description tiff:Orientation=\"3\"/></rdf:li></rdf:Bag></xmpMM:Pantry><tiff:Orientation>{o}</tiff:Orientation></rdf:Description></rdf:RDF></x:xmpmeta>"
        )
    };
    let f = jpeg(&[app1_xmp(x("6").as_bytes()), dqt()]);
    assert_eq!(run_reset(&f), jpeg(&[app1_xmp(x("1").as_bytes()), dqt()]));
}

// ---- errors and limits ----

#[test]
fn unknown_format() {
    assert_eq!(
        strip(b"not an image", BOTH, &limits(), &no_cancel()),
        Err(Error::UnknownFormat)
    );
    assert_eq!(
        reset_orientation(b"not an image", &limits(), &no_cancel()),
        Err(Error::UnknownFormat)
    );
}

#[test]
fn limits_and_cancel() {
    let files = [full_jpeg(false), full_png(false), full_webp(false)];
    for f in &files {
        let l = Limits {
            max_alloc: 10,
            ..limits()
        };
        assert_eq!(
            strip(f, NONE, &l, &no_cancel()),
            Err(Error::LimitExceeded(Limit::Alloc))
        );
        assert_eq!(
            reset_orientation(f, &l, &no_cancel()),
            Err(Error::LimitExceeded(Limit::Alloc))
        );
        let l = Limits {
            max_segments: 1,
            ..limits()
        };
        assert_eq!(
            strip(f, NONE, &l, &no_cancel()),
            Err(Error::LimitExceeded(Limit::Segments))
        );
        assert_eq!(
            reset_orientation(f, &l, &no_cancel()),
            Err(Error::LimitExceeded(Limit::Segments))
        );
        let l = Limits {
            max_exif_bytes: 5,
            ..limits()
        };
        assert_eq!(
            strip(f, ORIENTATION, &l, &no_cancel()),
            Err(Error::LimitExceeded(Limit::ExifBytes))
        );
        let flag = AtomicBool::new(true);
        assert_eq!(strip(f, NONE, &limits(), &flag), Err(Error::Cancelled));
        assert_eq!(
            reset_orientation(f, &limits(), &flag),
            Err(Error::Cancelled)
        );
    }
}

#[test]
fn output_is_charged_to_max_alloc() {
    let f = full_jpeg(false);
    let l = Limits {
        max_alloc: f.len() + 63,
        ..limits()
    };
    assert_eq!(
        strip(&f, NONE, &l, &no_cancel()),
        Err(Error::LimitExceeded(Limit::Alloc))
    );
    let l = Limits {
        max_alloc: f.len() + 64,
        ..limits()
    };
    assert!(strip(&f, NONE, &l, &no_cancel()).is_ok());
}

#[test]
fn orientation_of_type_ifd_is_kept_and_reset() {
    // ExifTool reads type 13 (IFD) as int32u.
    let t = tiff(
        false,
        &[Ent {
            tag: 0x0112,
            typ: 13,
            count: 1,
            data: 6u32.to_le_bytes().to_vec(),
        }],
    );
    let f = jpeg(&[app1_exif(&t), dqt()]);
    assert_eq!(orientation_of(&f), Some(6));
    assert_eq!(
        run_strip(&f, ORIENTATION),
        jpeg(&[app1_exif(&min_exif(false, 6)), dqt()])
    );
    let out = run_reset(&f);
    assert_eq!(orientation_of(&out), Some(1));
}

#[test]
fn only_the_first_orientation_entry_counts() {
    // ExifTool reports the first entry (priority 0). Not an integer: no orientation to keep, and
    // reset changes nothing (a later SHORT is not the one ExifTool reports).
    let t = tiff(false, &[ascii(0x0112, "6"), short(false, 0x0112, 3)]);
    let f = jpeg(&[app1_exif(&t), dqt()]);
    assert_eq!(orientation_of(&f), None);
    assert_eq!(run_strip(&f, ORIENTATION), jpeg(&[dqt()]));
    assert_eq!(run_reset(&f), f);
    // An integer out of the u16 range: no orientation; reset sets that first entry to 1.
    let long = Ent {
        tag: 0x0112,
        typ: 4,
        count: 1,
        data: 70_000u32.to_le_bytes().to_vec(),
    };
    let t = tiff(false, &[long, short(false, 0x0112, 5)]);
    let f = jpeg(&[app1_exif(&t), dqt()]);
    assert_eq!(orientation_of(&f), None);
    assert_eq!(run_strip(&f, ORIENTATION), jpeg(&[dqt()]));
    let out = run_reset(&f);
    assert_eq!(orientation_of(&out), Some(1));
    let mut want = f.clone();
    let at = f.windows(t.len()).position(|w| w == t).unwrap() + ORIENTATION_VALUE_AT;
    want[at..at + 4].copy_from_slice(&[1, 0, 0, 0]);
    assert_eq!(out, want);
}

#[test]
fn reset_reaches_an_orientation_in_a_continuation_segment() {
    // The EXIF block split over two APP1 segments (the second `Exif\0\0` with no TIFF header):
    // IFD0 wholly in the second one; and a BE LONG value split across the two.
    let long_be = |v: u32| {
        tiff(
            true,
            &[Ent {
                tag: 0x0112,
                typ: 4,
                count: 1,
                data: v.to_be_bytes().to_vec(),
            }],
        )
    };
    let cases = [
        (
            tiff(false, &[short(false, 0x0112, 6)]),
            tiff(false, &[short(false, 0x0112, 1)]),
            8,
        ),
        (long_be(6), long_be(1), 20),
    ];
    for (t, t1, split) in cases {
        let file = |b: &[u8]| jpeg(&[app1_exif(&b[..split]), app1_exif(&b[split..]), dqt()]);
        let f = file(&t);
        assert_eq!(orientation_of(&f), Some(6));
        let out = run_reset(&f);
        assert_eq!(out, file(&t1));
        assert_eq!(orientation_of(&out), Some(1));
    }
}

#[test]
fn joined_exif_capacity_is_released_before_the_output() {
    // A 4000-byte EXIF APP1 and a 10-byte continuation: joining doubles the capacity to 8000.
    // All of it is given back before the output buffer is charged.
    let mut t = tiff(false, &[short(false, 0x0112, 6)]);
    t.resize(4000, 0);
    let more = seg(0xE1, &[b"Exif\0\0".as_slice(), &[1; 10]].concat());
    let big = seg(0xDB, &[0x11; 60_000]);
    let f = jpeg(&[app1_exif(&t), more, big.clone()]);
    let l = Limits {
        max_alloc: f.len() + 64 + 1000,
        ..limits()
    };
    assert_eq!(
        strip(&f, ORIENTATION, &l, &no_cancel()),
        Ok(jpeg(&[app1_exif(&min_exif(false, 6)), big]))
    );
}

/// Fuzz crash (strip target, run 44498372865): an out-of-line SHORT[9] Orientation whose offset
/// points into the IFD0 directory. Writing 1 there changed the entry count, so a second reset
/// changed the file again. The value is now left alone, and reset is idempotent.
#[test]
fn reset_orientation_value_inside_ifd0_directory_is_idempotent() {
    let mut t = b"II*\0\x08\0\0\0\x02\0".to_vec();
    t.extend_from_slice(&[0x12, 0x01, 3, 0, 9, 0, 0, 0, 14, 0, 0, 0]);
    t.extend_from_slice(&[0x69, 0x87, 4, 0, 1, 0, 0, 0, 38, 0, 0, 0]);
    t.extend_from_slice(&[0, 0, 0, 0]);
    t.extend_from_slice(&[
        1, 0, 0x27, 0x88, 13, 0, 1, 0, 0, 0, 100, 0, 0, 0, 0, 0, 0, 0,
    ]);
    let file = jpeg(&[app1_exif(&t), dqt()]);
    let (l, c) = (limits(), no_cancel());
    let once = reset_orientation(&file, &l, &c).unwrap();
    let twice = reset_orientation(&once, &l, &c).unwrap();
    assert_eq!(once, twice);
    assert_eq!(once, file);
}
