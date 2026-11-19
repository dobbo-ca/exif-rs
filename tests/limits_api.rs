//! The `Limits` defaults match docs/limits.md, `Error::class` tokens are stable, and every
//! `Limit` variant is reachable through the public API.

use std::collections::HashSet;
use std::sync::atomic::AtomicBool;

use exif::{Error, Exif, Limit, Limits, Xmp};

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn entry(tag: u16, typ: u16, count: u32, val: u32) -> [u8; 12] {
    let mut e = [0u8; 12];
    e[0..2].copy_from_slice(&tag.to_le_bytes());
    e[2..4].copy_from_slice(&typ.to_le_bytes());
    e[4..8].copy_from_slice(&count.to_le_bytes());
    e[8..12].copy_from_slice(&val.to_le_bytes());
    e
}

/// An IFD: entry count, entries, next-IFD offset 0.
fn ifd(entries: &[[u8; 12]]) -> Vec<u8> {
    let mut v = (entries.len() as u16).to_le_bytes().to_vec();
    for e in entries {
        v.extend_from_slice(e);
    }
    v.extend_from_slice(&0u32.to_le_bytes());
    v
}

fn tiff(ifds: &[Vec<u8>]) -> Vec<u8> {
    let mut v = b"II*\0".to_vec();
    v.extend_from_slice(&8u32.to_le_bytes());
    for i in ifds {
        v.extend_from_slice(i);
    }
    v
}

/// IFD0 -> Exif IFD -> Interop IFD, one entry each (3 IFDs, depth 2). Each IFD is 18 bytes.
fn chain() -> Vec<u8> {
    tiff(&[
        ifd(&[entry(0x8769, 4, 1, 26)]),
        ifd(&[entry(0xA005, 4, 1, 44)]),
        ifd(&[entry(0x0001, 3, 1, 1)]),
    ])
}

/// IFD0 with 3 entries (one points to the Exif IFD at 8 + 2 + 36 + 4 = 50) and an Exif IFD with 3.
fn two_ifds_of_three() -> Vec<u8> {
    tiff(&[
        ifd(&[
            entry(0x0100, 3, 1, 1),
            entry(0x0101, 3, 1, 1),
            entry(0x8769, 4, 1, 50),
        ]),
        ifd(&[
            entry(0x9000, 3, 1, 1),
            entry(0x9001, 3, 1, 1),
            entry(0x9002, 3, 1, 1),
        ]),
    ])
}

fn many_entries() -> Vec<u8> {
    let es: Vec<_> = (0..10).map(|i| entry(0x1000 + i, 3, 1, 1)).collect();
    tiff(&[ifd(&es)])
}

/// One ASCII entry of 100 bytes, stored after the IFD at 8 + 18 = 26.
fn big_value() -> Vec<u8> {
    let mut t = tiff(&[ifd(&[entry(0x010F, 2, 100, 26)])]);
    t.resize(t.len() + 99, b'a');
    t.push(0);
    t
}

fn xmp(n: usize) -> Vec<u8> {
    let mut s = String::from(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
         <rdf:Description rdf:about=\"\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\">",
    );
    for i in 0..n {
        s.push_str(&format!("<xmp:P{i}>v</xmp:P{i}>"));
    }
    s.push_str("</rdf:Description></rdf:RDF></x:xmpmeta>");
    s.into_bytes()
}

fn segment(marker: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![0xFF, marker];
    v.extend_from_slice(&(body.len() as u16 + 2).to_be_bytes());
    v.extend_from_slice(body);
    v
}

/// SOI, the given segments, SOS with a little scan data, EOI.
fn jpeg(segments: &[Vec<u8>]) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8];
    for s in segments {
        v.extend_from_slice(s);
    }
    v.extend(segment(0xDA, &[1, 1, 0, 0, 63, 0]));
    v.extend_from_slice(&[0x55; 4]);
    v.extend_from_slice(&[0xFF, 0xD9]);
    v
}

fn app1_exif(tiff: &[u8]) -> Vec<u8> {
    segment(0xE1, &[&b"Exif\0\0"[..], tiff].concat())
}

#[test]
fn defaults_are_finite_and_match_docs() {
    let d = Limits::default();
    assert_eq!(d.max_alloc, 256 << 20);
    assert_eq!(
        d.max_threads,
        std::thread::available_parallelism().map_or(1, usize::from)
    );
    assert!(d.max_threads >= 1);
    assert_eq!(d.max_exif_bytes, 16 << 20);
    assert_eq!(d.max_xmp_bytes, 16 << 20);
    assert_eq!(d.max_ifds, 64);
    assert_eq!(d.max_entries, 4096);
    assert_eq!(d.max_total_entries, 65536);
    assert_eq!(d.max_depth, 8);
    assert_eq!(d.max_value_bytes, 64 << 20);
    assert_eq!(d.max_segments, 100_000);
    assert_eq!(d.max_xmp_nodes, 1_000_000);
    for (name, v) in [
        ("max_alloc", d.max_alloc),
        ("max_threads", d.max_threads),
        ("max_exif_bytes", d.max_exif_bytes),
        ("max_xmp_bytes", d.max_xmp_bytes),
        ("max_ifds", d.max_ifds),
        ("max_entries", d.max_entries),
        ("max_total_entries", d.max_total_entries),
        ("max_depth", d.max_depth),
        ("max_value_bytes", d.max_value_bytes),
        ("max_segments", d.max_segments),
        ("max_xmp_nodes", d.max_xmp_nodes),
    ] {
        assert!(v < usize::MAX / 2, "{name} is not finite in practice: {v}");
    }
}

/// Exhaustive on purpose: a new `Limit` variant fails to compile here until it is covered.
fn token(l: Limit) -> &'static str {
    match l {
        Limit::Alloc => "limit-alloc",
        Limit::ExifBytes => "limit-exif-bytes",
        Limit::XmpBytes => "limit-xmp-bytes",
        Limit::Ifds => "limit-ifds",
        Limit::Entries => "limit-entries",
        Limit::TotalEntries => "limit-total-entries",
        Limit::Depth => "limit-depth",
        Limit::ValueBytes => "limit-value-bytes",
        Limit::Segments => "limit-segments",
        Limit::XmpNodes => "limit-xmp-nodes",
    }
}

#[test]
fn error_class_tokens() {
    assert_eq!(Error::UnknownFormat.class(), "unknown-format");
    assert_eq!(Error::Truncated.class(), "truncated");
    assert_eq!(Error::Malformed("png chunk crc").class(), "malformed");
    assert_eq!(Error::OutOfMemory.class(), "out-of-memory");
    assert_eq!(Error::Cancelled.class(), "cancelled");
    for l in [
        Limit::Alloc,
        Limit::ExifBytes,
        Limit::XmpBytes,
        Limit::Ifds,
        Limit::Entries,
        Limit::TotalEntries,
        Limit::Depth,
        Limit::ValueBytes,
        Limit::Segments,
        Limit::XmpNodes,
    ] {
        assert_eq!(Error::LimitExceeded(l).class(), token(l));
    }
}

#[test]
fn every_limit_is_reachable_through_the_public_api() {
    let c = no_cancel();
    let d = Limits::default();
    let exif_jpeg = jpeg(&[app1_exif(&many_entries())]);
    let coms: Vec<_> = (0..10).map(|_| segment(0xFE, b"x")).collect();

    let cases: Vec<(Limit, Result<(), Error>)> = vec![
        (
            Limit::Alloc,
            exif::read(&exif_jpeg, &Limits { max_alloc: 16, ..d }, &c).map(|_| ()),
        ),
        (
            Limit::ExifBytes,
            exif::read(
                &exif_jpeg,
                &Limits {
                    max_exif_bytes: 32,
                    ..d
                },
                &c,
            )
            .map(|_| ()),
        ),
        (
            Limit::XmpBytes,
            Xmp::parse(
                &xmp(30),
                None,
                &Limits {
                    max_xmp_bytes: 64,
                    ..d
                },
                &c,
            )
            .map(|_| ()),
        ),
        (
            Limit::Ifds,
            Exif::parse(&chain(), &Limits { max_ifds: 1, ..d }, &c).map(|_| ()),
        ),
        (
            Limit::Entries,
            Exif::parse(
                &many_entries(),
                &Limits {
                    max_entries: 4,
                    ..d
                },
                &c,
            )
            .map(|_| ()),
        ),
        (
            Limit::TotalEntries,
            Exif::parse(
                &two_ifds_of_three(),
                &Limits {
                    max_total_entries: 4,
                    ..d
                },
                &c,
            )
            .map(|_| ()),
        ),
        (
            Limit::Depth,
            Exif::parse(&chain(), &Limits { max_depth: 0, ..d }, &c).map(|_| ()),
        ),
        (
            Limit::ValueBytes,
            Exif::parse(
                &big_value(),
                &Limits {
                    max_value_bytes: 16,
                    ..d
                },
                &c,
            )
            .map(|_| ()),
        ),
        (
            Limit::Segments,
            exif::read(
                &jpeg(&coms),
                &Limits {
                    max_segments: 3,
                    ..d
                },
                &c,
            )
            .map(|_| ()),
        ),
        (
            Limit::XmpNodes,
            Xmp::parse(
                &xmp(50),
                None,
                &Limits {
                    max_xmp_nodes: 5,
                    ..d
                },
                &c,
            )
            .map(|_| ()),
        ),
    ];

    let mut seen = HashSet::new();
    for (limit, got) in cases {
        let want = Err(Error::LimitExceeded(limit));
        assert_eq!(got, want, "{limit:?}");
        assert_eq!(want.map_err(|e| e.class()), Err(token(limit)));
        seen.insert(limit);
    }
    assert_eq!(seen.len(), 10, "every Limit variant has a case");

    // The same inputs pass under the default limits, so each error above came from its field.
    assert!(exif::read(&exif_jpeg, &d, &c).is_ok());
    assert!(Exif::parse(&chain(), &d, &c).is_ok());
    assert!(Exif::parse(&two_ifds_of_three(), &d, &c).is_ok());
    assert!(Exif::parse(&big_value(), &d, &c).is_ok());
    assert!(Xmp::parse(&xmp(50), None, &d, &c).is_ok());
    assert!(exif::read(&jpeg(&coms), &d, &c).is_ok());
}

#[test]
fn unknown_format_and_cancel() {
    let d = Limits::default();
    assert_eq!(
        exif::read(b"not an image file at all", &d, &no_cancel()).map(|_| ()),
        Err(Error::UnknownFormat)
    );
    let cancelled = AtomicBool::new(true);
    let coms: Vec<_> = (0..10).map(|_| segment(0xFE, b"x")).collect();
    assert_eq!(
        exif::read(&jpeg(&coms), &d, &cancelled).map(|_| ()),
        Err(Error::Cancelled)
    );
}
