//! strip and reset_orientation of JPEG files with data after the primary image's EOI: an Ultra
//! HDR gain map (MPF), a Motion Photo video, both, and hostile MPF segments and directories.

mod common;

use common::*;
use exif::{IfdKind, Keep, read, reset_orientation, strip};

const NS_CONTAINER_ALL: [&str; 4] = [
    "http://ns.adobe.com/hdr-gain-map/1.0/",
    "http://ns.google.com/photos/1.0/camera/",
    "http://ns.google.com/photos/1.0/container/",
    "http://ns.google.com/photos/1.0/container/item/",
];
const ISO: &[u8] = b"urn:iso:std:iso:ts:21496:-1\0";
const VIDEO: &[u8] = b"\0\0\0\x10ftypmp42\0\0\0\0fake video bytes \xFF\xD8\xFF\xD9";

fn u16b(be: bool, x: u16) -> [u8; 2] {
    if be { x.to_be_bytes() } else { x.to_le_bytes() }
}

fn u32b(be: bool, x: u32) -> [u8; 4] {
    if be { x.to_be_bytes() } else { x.to_le_bytes() }
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// IFD0 with Orientation and a GPS IFD pointer; the GPS IFD (at 38) holds GPSLatitudeRef `N`.
fn exif_with_gps(be: bool, orientation: u16) -> Vec<u8> {
    let mut t = tiff(
        be,
        &[
            short(be, 0x0112, orientation),
            Ent {
                tag: 0x8825,
                typ: 4,
                count: 1,
                data: u32b(be, 38).to_vec(),
            },
        ],
    );
    assert_eq!(t.len(), 38);
    t.extend_from_slice(&u16b(be, 1));
    t.extend_from_slice(&u16b(be, 1));
    t.extend_from_slice(&u16b(be, 2));
    t.extend_from_slice(&u32b(be, 2));
    t.extend_from_slice(b"N\0\0\0");
    t.extend_from_slice(&[0; 4]);
    t
}

/// An APP2 MPF segment with MPFVersion, NumberOfImages and the MP Entries (attribute, size,
/// offset); the entries start 50 bytes after the TIFF header.
fn mpf_seg(be: bool, ents: &[(u32, u32, u32)]) -> Vec<u8> {
    let n = ents.len() as u32;
    let mut p = b"MPF\0".to_vec();
    p.extend_from_slice(if be { b"MM\0*" } else { b"II*\0" });
    p.extend_from_slice(&u32b(be, 8));
    p.extend_from_slice(&u16b(be, 3));
    for (tag, typ, count, value) in [
        (0xB000, 7, 4, *b"0100"),
        (0xB001, 4, 1, u32b(be, n)),
        (0xB002, 7, 16 * n, u32b(be, 50)),
    ] {
        p.extend_from_slice(&u16b(be, tag));
        p.extend_from_slice(&u16b(be, typ));
        p.extend_from_slice(&u32b(be, count));
        p.extend_from_slice(&value);
    }
    p.extend_from_slice(&[0; 4]);
    assert_eq!(p.len(), 4 + 50);
    for &(attr, size, off) in ents {
        p.extend_from_slice(&u32b(be, attr));
        p.extend_from_slice(&u32b(be, size));
        p.extend_from_slice(&u32b(be, off));
        p.extend_from_slice(&[0; 4]);
    }
    seg(0xE2, &p)
}

/// The TIFF header offset of the first MPF segment of `f` and its entries (size, offset).
fn mpf_entries(f: &[u8]) -> (usize, Vec<(u32, u32)>) {
    let tiff = find(f, b"MPF\0").expect("no MPF") + 4;
    let be = &f[tiff..tiff + 2] == b"MM";
    let r16 = |at: usize| {
        let b = [f[at], f[at + 1]];
        if be {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        }
    };
    let r32 = |at: usize| {
        let b: [u8; 4] = f[at..at + 4].try_into().unwrap();
        if be {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        }
    };
    let ifd = tiff + r32(tiff + 4) as usize;
    for k in 0..usize::from(r16(ifd)) {
        let e = ifd + 2 + 12 * k;
        if r16(e) == 0xB002 {
            let at = tiff + r32(e + 8) as usize;
            let ents = (0..r32(e + 4) as usize / 16)
                .map(|i| (r32(at + 16 * i + 4), r32(at + 16 * i + 8)))
                .collect();
            return (tiff, ents);
        }
    }
    panic!("no MP Entry tag");
}

/// The primary image's XMP packet: orientation, a GPS field and a creator (dropped by strip),
/// and with a gain map `hdrgm:Version`, with a video the Motion Photo fields; the directory lists
/// the primary, the gain map (attributes) and the video (child elements). `pre` are the source
/// prefixes of the camera, container and item namespaces.
fn primary_xmp(
    gm: Option<usize>,
    video: Option<usize>,
    pre: [&str; 3],
    orientation: &str,
) -> String {
    let [cam, con, item] = pre;
    let mut attrs = format!(" tiff:Orientation=\"{orientation}\" exif:GPSLatitudeRef=\"N\"");
    if gm.is_some() {
        attrs.push_str(" hdrgm:Version=\"1.0\"");
    }
    if video.is_some() {
        attrs.push_str(&format!(
            " {cam}:MotionPhoto=\"1\" {cam}:MotionPhotoVersion=\"1\" \
             {cam}:MotionPhotoPresentationTimestampUs=\"-1\""
        ));
    }
    let pad = if video.is_some() {
        format!(" {item}:Padding=\"0\"")
    } else {
        String::new()
    };
    let mut items = format!(
        "<rdf:li rdf:parseType=\"Resource\"><{con}:Item {item}:Semantic=\"Primary\" \
         {item}:Mime=\"image/jpeg\"{pad}/></rdf:li>"
    );
    if let Some(n) = gm {
        items.push_str(&format!(
            "<rdf:li rdf:parseType=\"Resource\"><{con}:Item {item}:Semantic=\"GainMap\" \
             {item}:Mime=\"image/jpeg\" {item}:Length=\"{n}\"/></rdf:li>"
        ));
    }
    if let Some(n) = video {
        items.push_str(&format!(
            "<rdf:li rdf:parseType=\"Resource\"><{con}:Item rdf:parseType=\"Resource\">\
             <{item}:Mime>video/mp4</{item}:Mime><{item}:Semantic>MotionPhoto</{item}:Semantic>\
             <{item}:Length> {n} </{item}:Length></{con}:Item></rdf:li>"
        ));
    }
    format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
         xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" \
         xmlns:tiff=\"http://ns.adobe.com/tiff/1.0/\" xmlns:exif=\"http://ns.adobe.com/exif/1.0/\" \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:hdrgm=\"http://ns.adobe.com/hdr-gain-map/1.0/\" \
         xmlns:{cam}=\"http://ns.google.com/photos/1.0/camera/\" \
         xmlns:{con}=\"http://ns.google.com/photos/1.0/container/\" \
         xmlns:{item}=\"http://ns.google.com/photos/1.0/container/item/\"{attrs}>\
         <dc:creator><rdf:Seq><rdf:li>Someone</rdf:li></rdf:Seq></dc:creator>\
         <{con}:Directory><rdf:Seq>{items}</rdf:Seq></{con}:Directory>\
         </rdf:Description></rdf:RDF></x:xmpmeta>"
    )
}

/// A gain map JPEG: its own XMP and ISO 21496-1 metadata, a table, the fake scan.
fn gain_map() -> Vec<u8> {
    let xmp = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
               xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description \
               xmlns:hdrgm=\"http://ns.adobe.com/hdr-gain-map/1.0/\" hdrgm:Version=\"1.0\" \
               hdrgm:GainMapMax=\"2.0\"/></rdf:RDF></x:xmpmeta>";
    jpeg(&[
        app1_xmp(xmp.as_bytes()),
        seg(0xE2, &[ISO, &[0, 0, 0, 0, 1, 2, 3, 4][..]].concat()),
        dqt(),
    ])
}

fn iso_primary() -> Vec<u8> {
    seg(0xE2, &[ISO, &[0, 0, 0, 0][..]].concat())
}

struct Photo {
    file: Vec<u8>,
    gm: Vec<u8>,
    video: Vec<u8>,
}

impl Photo {
    /// What follows the primary image.
    fn tail(&self) -> Vec<u8> {
        [&self.gm[..], &self.video[..]].concat()
    }
}

const PREFIXES: [[&str; 3]; 2] = [
    ["Camera", "Container", "Item"],
    ["GCamera", "GContainer", "GItem"],
];

/// A primary image (SOI, JFIF, EXIF with orientation 6 and GPS, XMP, ICC, ISO 21496-1, DQT, scan;
/// with a gain map an MPF segment after JFIF or right before the scan), then the gain map and
/// the video.
fn photo(be: bool, mpf_first: bool, with_gm: bool, video: &[u8], orientation: &str) -> Photo {
    let gm = if with_gm { gain_map() } else { Vec::new() };
    let pre = PREFIXES[usize::from(be)];
    let xmp = primary_xmp(
        with_gm.then_some(gm.len()),
        (!video.is_empty()).then_some(video.len()),
        pre,
        orientation,
    );
    let build = |mpf: Vec<u8>| {
        let mut segs = vec![app0_jfif()];
        if with_gm && mpf_first {
            segs.push(mpf.clone());
        }
        segs.push(app1_exif(&exif_with_gps(be, 6)));
        segs.push(app1_xmp(xmp.as_bytes()));
        segs.push(app2_icc(1, 1, b"icc!"));
        segs.push(iso_primary());
        segs.push(dqt());
        if with_gm && !mpf_first {
            segs.push(mpf);
        }
        jpeg(&segs)
    };
    let gl = gm.len() as u32;
    let mut primary = build(mpf_seg(be, &[(0x0003_0000, 0, 0), (0, gl, 0)]));
    if with_gm {
        let tiff = find(&primary, b"MPF\0").unwrap() + 4;
        let (pl, off) = (primary.len() as u32, (primary.len() - tiff) as u32);
        primary = build(mpf_seg(be, &[(0x0003_0000, pl, 0), (0, gl, off)]));
    }
    let file = [&primary[..], &gm[..], video].concat();
    Photo {
        file,
        gm,
        video: video.to_vec(),
    }
}

fn run(f: &[u8], keep: Keep) -> Vec<u8> {
    strip(f, keep, &limits(), &no_cancel()).unwrap()
}

fn reset(f: &[u8]) -> Vec<u8> {
    reset_orientation(f, &limits(), &no_cancel()).unwrap()
}

/// `out` ends with the gain map and the video of `p`, and its MPF segment gives the primary its
/// new length and points at the gain map's SOI.
fn check_tail(out: &[u8], p: &Photo, at: &str) {
    let tail = p.tail();
    assert!(out.ends_with(&tail), "{at}: tail");
    let primary = out.len() - tail.len();
    if p.gm.is_empty() {
        assert!(find(out, b"MPF\0").is_none(), "{at}: MPF");
        return;
    }
    let (tiff, ents) = mpf_entries(out);
    assert_eq!(ents.len(), 2, "{at}");
    assert_eq!(ents[0], (primary as u32, 0), "{at}: primary entry");
    assert_eq!(
        ents[1],
        (p.gm.len() as u32, (primary - tiff) as u32),
        "{at}: gain map entry"
    );
    assert_eq!(out[primary..primary + 2], [0xFF, 0xD8], "{at}");
}

/// The XMP packet of a stripped file: only the container namespaces.
fn kept_packet(out: &[u8], at: &str) -> String {
    let m = read(out, &limits(), &no_cancel()).unwrap();
    let x = m.xmp.expect("no XMP");
    for p in x.properties() {
        assert!(NS_CONTAINER_ALL.contains(&p.ns.as_str()), "{at}: {p:?}");
    }
    let packet = String::from_utf8(m.raw.xmp.unwrap()).unwrap();
    for gone in [
        "tiff:",
        "exif:",
        "dc:",
        "Someone",
        "GCamera",
        "GContainer",
        "GItem",
    ] {
        assert!(!packet.contains(gone), "{at}: {gone} in {packet}");
    }
    packet
}

fn check_exif_orientation_only(out: &[u8], at: &str) {
    let m = read(out, &limits(), &no_cancel()).unwrap();
    let e = m.exif.expect("no EXIF");
    assert_eq!(e.orientation(), Some(6), "{at}");
    assert!(e.gps().is_none(), "{at}: GPS");
    assert!(
        e.entries(IfdKind::Ifd0).iter().all(|n| n.tag == 0x0112),
        "{at}"
    );
}

fn check_reset(p: &Photo, shift: usize, at: &str) {
    let out = reset(&p.file);
    assert_eq!(out.len(), p.file.len() - shift, "{at}");
    check_tail(&out, p, at);
    let m = read(&out, &limits(), &no_cancel()).unwrap();
    let e = m.exif.unwrap();
    assert_eq!(e.orientation(), Some(1), "{at}");
    assert!(e.gps().is_some(), "{at}: reset keeps GPS");
    assert_eq!(m.xmp.unwrap().orientation(), Some(1), "{at}");
}

// (1) Ultra HDR: primary, MPF in either place and byte order, gain map after the EOI.
#[test]
fn ultra_hdr() {
    for be in [false, true] {
        for mpf_first in [false, true] {
            let at = format!("be={be} mpf_first={mpf_first}");
            let p = photo(be, mpf_first, true, b"", "6");
            let (_, ents) = mpf_entries(&p.file);
            assert_eq!(ents[0].0 as usize, p.file.len() - p.gm.len(), "{at}: input");
            let out = run(&p.file, Keep::default());
            check_tail(&out, &p, &at);
            check_exif_orientation_only(&out, &at);
            let packet = kept_packet(&out, &at);
            assert!(packet.contains("hdrgm:Version=\"1.0\""), "{at}: {packet}");
            assert!(packet.contains("<Container:Directory><rdf:Seq>"), "{at}");
            assert!(packet.contains("Item:Semantic=\"Primary\""), "{at}");
            let gm = format!(
                "Item:Mime=\"image/jpeg\" Item:Semantic=\"GainMap\" Item:Length=\"{}\"",
                p.gm.len()
            );
            assert!(packet.contains(&gm), "{at}: {packet}");
            assert!(!packet.contains("Camera:"), "{at}");
            // ICC and the ISO 21496-1 APP2 stay.
            assert!(find(&out, b"ICC_PROFILE\0").is_some(), "{at}");
            assert!(find(&out[..out.len() - p.gm.len()], ISO).is_some(), "{at}");

            // The XMP value " 6" becomes "1": everything after it moves back one byte.
            check_reset(&p, 0, &format!("{at} reset"));
            let p = photo(be, mpf_first, true, b"", " 6");
            check_reset(&p, 1, &format!("{at} reset shift"));
        }
    }
}

// (2) Motion Photo: no MPF, the video after the EOI.
#[test]
fn motion_photo() {
    for be in [false, true] {
        let at = format!("be={be}");
        let p = photo(be, false, false, VIDEO, "6");
        let out = run(&p.file, Keep::default());
        check_tail(&out, &p, &at);
        check_exif_orientation_only(&out, &at);
        let packet = kept_packet(&out, &at);
        for want in [
            "Camera:MotionPhoto=\"1\"".to_string(),
            "Camera:MotionPhotoVersion=\"1\"".to_string(),
            "Camera:MotionPhotoPresentationTimestampUs=\"-1\"".to_string(),
            "Item:Mime=\"image/jpeg\" Item:Semantic=\"Primary\" Item:Padding=\"0\"".to_string(),
            format!(
                "Item:Mime=\"video/mp4\" Item:Semantic=\"MotionPhoto\" Item:Length=\" {} \"",
                VIDEO.len()
            ),
        ] {
            assert!(packet.contains(&want), "{at}: {want} not in {packet}");
        }
        assert!(!packet.contains("hdrgm:Version"), "{at}");
        check_reset(&p, 0, &format!("{at} reset"));
        check_reset(
            &photo(be, false, false, VIDEO, " 6"),
            1,
            &format!("{at} reset shift"),
        );
    }
}

// (3) Both: primary, gain map, video.
#[test]
fn ultra_hdr_motion_photo() {
    for be in [false, true] {
        for mpf_first in [false, true] {
            let at = format!("be={be} mpf_first={mpf_first}");
            let p = photo(be, mpf_first, true, VIDEO, "6");
            let out = run(&p.file, Keep::default());
            check_tail(&out, &p, &at);
            // Keep-lists that keep the MPF images but drop a segment (ICC, the ISO APP2, the
            // EXIF) that lies between the MPF header and the gain map when mpf_first.
            for keep in [
                Keep {
                    icc: false,
                    ..Keep::default()
                },
                Keep {
                    unknown: false,
                    ..Keep::default()
                },
                Keep {
                    orientation: false,
                    ..Keep::default()
                },
            ] {
                check_tail(&run(&p.file, keep), &p, &format!("{at} {keep:?}"));
            }
            let packet = kept_packet(&out, &at);
            for want in [
                "hdrgm:Version",
                "Semantic=\"GainMap\"",
                "Semantic=\"MotionPhoto\"",
            ] {
                assert!(packet.contains(want), "{at}: {want}");
            }
            check_reset(&p, 0, &format!("{at} reset"));
            check_reset(
                &photo(be, mpf_first, true, VIDEO, " 6"),
                1,
                &format!("{at} shift"),
            );
        }
    }
}

/// What strip keeps of the primary image of [`photo`] with neither trailing data nor XMP: JFIF,
/// the minimal EXIF, ICC, the ISO APP2, the table.
fn bare_primary(be: bool) -> Vec<u8> {
    jpeg(&[
        app0_jfif(),
        app1_exif(&tiff(be, &[short(be, 0x0112, 6)])),
        app2_icc(1, 1, b"icc!"),
        iso_primary(),
        dqt(),
    ])
}

// (4) Without `trailing` nothing after the EOI stays, nor the MPF segment and the container XMP;
// without `mpf_images` the gain map goes, the directory loses its item and hdrgm:Version goes.
#[test]
fn keep_trailing_and_mpf_images_off() {
    for be in [false, true] {
        for mpf_first in [false, true] {
            let at = format!("be={be} mpf_first={mpf_first}");
            for (gm, video) in [(true, &b""[..]), (false, VIDEO), (true, VIDEO)] {
                let p = photo(be, mpf_first, gm, video, "6");
                let no_trailing = Keep {
                    trailing: false,
                    ..Keep::default()
                };
                assert_eq!(run(&p.file, no_trailing), bare_primary(be), "{at}");
            }

            let no_mpf = Keep {
                mpf_images: false,
                ..Keep::default()
            };
            // The gain map was all that followed: nothing is left to describe.
            let p = photo(be, mpf_first, true, b"", "6");
            assert_eq!(run(&p.file, no_mpf), bare_primary(be), "{at}");

            let p = photo(be, mpf_first, true, VIDEO, "6");
            let out = run(&p.file, no_mpf);
            assert!(out.ends_with(VIDEO), "{at}");
            assert!(!out.ends_with(&p.tail()), "{at}");
            assert!(find(&out, b"MPF\0").is_none(), "{at}");
            let packet = kept_packet(&out, &at);
            assert!(!packet.contains("hdrgm:Version"), "{at}: {packet}");
            assert!(!packet.contains("GainMap"), "{at}: {packet}");
            assert!(packet.contains("Semantic=\"Primary\""), "{at}");
            assert!(packet.contains("Semantic=\"MotionPhoto\""), "{at}");
            assert!(packet.contains("Camera:MotionPhoto=\"1\""), "{at}");
            let primary = out.len() - VIDEO.len();
            let xmp_seg = seg(
                0xE1,
                &[
                    b"http://ns.adobe.com/xap/1.0/\0".as_slice(),
                    packet.as_bytes(),
                ]
                .concat(),
            );
            let mut want = bare_primary(be);
            let after_exif =
                2 + app0_jfif().len() + app1_exif(&tiff(be, &[short(be, 0x0112, 6)])).len();
            let rest = want.split_off(after_exif);
            want.extend_from_slice(&xmp_seg);
            want.extend_from_slice(&rest);
            assert_eq!(out[..primary], want[..], "{at}");
        }
    }
}

// (5) Keep::NONE gives the v1 output: JFIF and the table, nothing after the EOI.
#[test]
fn keep_none_is_v1() {
    for be in [false, true] {
        for (gm, video) in [(true, &b""[..]), (false, VIDEO), (true, VIDEO)] {
            let p = photo(be, true, gm, video, "6");
            assert_eq!(run(&p.file, Keep::NONE), jpeg(&[app0_jfif(), dqt()]));
        }
    }
    let mut f = jpeg(&[seg(0xE5, b"junk")]);
    f.extend_from_slice(b"trailer after EOI");
    assert_eq!(run(&f, Keep::NONE), jpeg(&[]));
    assert_eq!(run(&f, Keep::default()), f);
}

// Which APPn segments are metadata (always dropped) and which are unknown (kept by default).
#[test]
fn unknown_segments() {
    let unknown = [
        seg(0xE0, b"JFXX\0\x10thumb"),
        seg(0xE1, b"FLIR\0\x01\x01"),
        seg(0xE3, b"Meta\0\0"),
        seg(0xEB, b"JP\0\0\0\0\0\0jumbf box"),
        seg(0xED, b"Adobe_CM\0"),
        seg(0xEE, b"NotAdobe"),
    ];
    let metadata = [
        seg(0xE1, b"Exif\0\xFFnot TIFF"),
        seg(0xE1, b"\0\0EXIF\0\0"),
        seg(0xE1, b"XMP\0<x/>"),
        seg(0xE1, b"http://ns.example.com/\0<x/>"),
        seg(0xE1, b"junk <?xpacket begin=''?>"),
        seg(0xED, b"Photoshop 3.0\08BIM"),
        seg(0xED, b"Adobe_Photoshop2.5:8BIM"),
        seg(0xFE, b"comment"),
    ];
    let mut segs = vec![app0_jfif()];
    for (u, m) in unknown.iter().zip(&metadata) {
        segs.push(m.clone());
        segs.push(u.clone());
    }
    segs.extend_from_slice(&metadata[unknown.len()..]);
    segs.push(dqt());
    let f = jpeg(&segs);
    let mut want = vec![app0_jfif()];
    want.extend_from_slice(&unknown);
    want.push(dqt());
    assert_eq!(run(&f, Keep::default()), jpeg(&want));
    assert_eq!(run(&f, Keep::NONE), jpeg(&[app0_jfif(), dqt()]));
}

// (6) Hostile MPF segments and directories: no panic, no error, nothing patched that is not valid.
#[test]
fn hostile_mpf_and_directory() {
    for be in [false, true] {
        let p = photo(be, false, true, VIDEO, "6");
        let tiff = find(&p.file, b"MPF\0").unwrap() + 4;
        let r32_at =
            |f: &mut Vec<u8>, at: usize, v: u32| f[at..at + 4].copy_from_slice(&u32b(be, v));
        // The MP Entry count is not a multiple of 16: the segment is kept as it is.
        let mut f = p.file.clone();
        r32_at(&mut f, tiff + 10 + 24 + 4, 33);
        let seg_at = tiff - 8;
        let seg_len = 4 + 50 + 32 + 4;
        let out = run(&f, Keep::default());
        assert!(out.ends_with(&p.tail()));
        let o = find(&out, b"MPF\0").unwrap() - 4;
        assert_eq!(out[o..o + seg_len], f[seg_at..seg_at + seg_len]);
        // Offsets out of the file, and one in the primary image: left as they are, not dropped.
        for off in [u32::MAX, 0x7FFF_FFF0, 16] {
            let mut f = p.file.clone();
            r32_at(&mut f, tiff + 50 + 16 + 8, off);
            let out = run(&f, Keep::default());
            assert_eq!(mpf_entries(&out).1[1].1, off);
            let no_mpf = Keep {
                mpf_images: false,
                ..Keep::default()
            };
            let out = run(&f, no_mpf);
            assert!(out.ends_with(&p.tail()), "off {off}");
        }
        // Absurd directory lengths: the gain map is dropped, the directory cannot be matched and
        // is left out; the Motion Photo fields stay.
        let gl = p.gm.len().to_string();
        let width = gl.len();
        let needle = format!("{}:Length=\"{gl}\"", PREFIXES[usize::from(be)][2]);
        let i = find(&p.file, needle.as_bytes()).unwrap() + needle.len() - 1 - width;
        for bad in ["999", "-5", "x", "0"] {
            // The same width, so nothing else moves.
            let padded = format!("{bad:>width$}");
            assert_eq!(padded.len(), width);
            let mut f = p.file.clone();
            f[i..i + width].copy_from_slice(padded.as_bytes());
            let out = run(
                &f,
                Keep {
                    mpf_images: false,
                    ..Keep::default()
                },
            );
            assert!(out.ends_with(VIDEO));
            assert!(!out.ends_with(&p.tail()));
            let packet = kept_packet(&out, bad);
            assert!(!packet.contains("Directory"), "{bad}: {packet}");
            assert!(packet.contains("Camera:MotionPhoto=\"1\""));
        }
        // Every cut of the file, every keep.
        let keeps = [
            Keep::default(),
            Keep::NONE,
            Keep {
                mpf_images: false,
                ..Keep::default()
            },
        ];
        for n in 0..=p.file.len() {
            for keep in keeps {
                let _ = strip(&p.file[..n], keep, &limits(), &no_cancel());
            }
            let _ = reset_orientation(&p.file[..n], &limits(), &no_cancel());
        }
    }
}
