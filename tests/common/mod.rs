//! Synthetic JPEG, PNG and WebP files for the tests, built in code (no binary fixtures).
#![allow(dead_code)]

use std::sync::atomic::AtomicBool;

use exif::Limits;

pub fn limits() -> Limits {
    Limits::default()
}

pub fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

// ---- TIFF / EXIF ----

/// One IFD entry: `data` is the value bytes in the block's byte order.
pub struct Ent {
    pub tag: u16,
    pub typ: u16,
    pub count: u32,
    pub data: Vec<u8>,
}

fn u16b(be: bool, x: u16) -> [u8; 2] {
    if be { x.to_be_bytes() } else { x.to_le_bytes() }
}

fn u32b(be: bool, x: u32) -> [u8; 4] {
    if be { x.to_be_bytes() } else { x.to_le_bytes() }
}

pub fn short(be: bool, tag: u16, v: u16) -> Ent {
    Ent {
        tag,
        typ: 3,
        count: 1,
        data: u16b(be, v).to_vec(),
    }
}

pub fn ascii(tag: u16, s: &str) -> Ent {
    let mut data = s.as_bytes().to_vec();
    data.push(0);
    Ent {
        tag,
        typ: 2,
        count: data.len() as u32,
        data,
    }
}

/// A TIFF block with one IFD (entries in the order given, sorted by tag by the caller).
/// `tiff(be, &[short(be, 0x0112, v)])` is 26 bytes: the same as the minimal block that
/// `strip` writes.
pub fn tiff(be: bool, ents: &[Ent]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(if be { b"MM\0*" } else { b"II*\0" });
    out.extend_from_slice(&u32b(be, 8));
    out.extend_from_slice(&u16b(be, ents.len() as u16));
    let data_start = 8 + 2 + 12 * ents.len() + 4;
    let mut extra: Vec<u8> = Vec::new();
    for e in ents {
        out.extend_from_slice(&u16b(be, e.tag));
        out.extend_from_slice(&u16b(be, e.typ));
        out.extend_from_slice(&u32b(be, e.count));
        if e.data.len() <= 4 {
            out.extend_from_slice(&e.data);
            out.extend_from_slice(&[0u8; 4][..4 - e.data.len()]);
        } else {
            out.extend_from_slice(&u32b(be, (data_start + extra.len()) as u32));
            extra.extend_from_slice(&e.data);
            if extra.len() % 2 == 1 {
                extra.push(0);
            }
        }
    }
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&extra);
    out
}

/// The offset of the orientation value in a block made by `tiff` whose first entry is the
/// orientation.
pub const ORIENTATION_VALUE_AT: usize = 8 + 2 + 8;

// ---- JPEG ----

pub fn seg(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = vec![0xFF, marker];
    v.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    v.extend_from_slice(payload);
    v
}

pub fn app0_jfif() -> Vec<u8> {
    seg(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0")
}

pub fn dqt() -> Vec<u8> {
    seg(0xDB, &[0x11; 65])
}

pub fn app1_exif(tiff: &[u8]) -> Vec<u8> {
    seg(0xE1, &[b"Exif\0\0".as_slice(), tiff].concat())
}

pub fn app1_xmp(packet: &[u8]) -> Vec<u8> {
    seg(
        0xE1,
        &[b"http://ns.adobe.com/xap/1.0/\0".as_slice(), packet].concat(),
    )
}

pub fn app1_ext(guid: &str, full: u32, off: u32, data: &[u8]) -> Vec<u8> {
    assert_eq!(guid.len(), 32);
    seg(
        0xE1,
        &[
            b"http://ns.adobe.com/xmp/extension/\0".as_slice(),
            guid.as_bytes(),
            &full.to_be_bytes(),
            &off.to_be_bytes(),
            data,
        ]
        .concat(),
    )
}

pub fn app2_icc(seq: u8, count: u8, data: &[u8]) -> Vec<u8> {
    seg(
        0xE2,
        &[b"ICC_PROFILE\0".as_slice(), &[seq, count], data].concat(),
    )
}

/// SOS header, entropy-coded data with a stuffed FF, an RSTn marker, and EOI.
pub const SCAN: &[u8] = &[
    0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0x12, 0x34, 0xFF, 0x00, 0x56, 0xFF,
    0xD0, 0x78, 0x9A, 0xFF, 0xD9,
];

/// SOI, the segments, then the fake scan.
pub fn jpeg(segs: &[Vec<u8>]) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8];
    for s in segs {
        v.extend_from_slice(s);
    }
    v.extend_from_slice(SCAN);
    v
}

/// The offset of the first SOS.
pub fn sos_at(file: &[u8]) -> usize {
    file.windows(2)
        .position(|w| w == [0xFF, 0xDA])
        .expect("no SOS")
}

// ---- PNG ----

pub const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// An independent bitwise CRC-32 (the crate's table version is under test).
pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = !0u32;
    for p in parts {
        for &b in *p {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 == 1 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
    }
    !c
}

pub fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = (data.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(data);
    v.extend_from_slice(&crc32(&[&kind[..], data]).to_be_bytes());
    v
}

pub fn png(chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut v = PNG_SIG.to_vec();
    for c in chunks {
        v.extend_from_slice(c);
    }
    v
}

pub fn ihdr() -> Vec<u8> {
    chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0])
}

pub fn idat() -> Vec<u8> {
    chunk(b"IDAT", &[0x78, 0x01, 1, 2, 3, 4, 5])
}

pub fn iend() -> Vec<u8> {
    chunk(b"IEND", &[])
}

/// IHDR, `extra`, IDAT, IEND.
pub fn png_with(extra: &[Vec<u8>]) -> Vec<u8> {
    let mut chunks = vec![ihdr()];
    chunks.extend_from_slice(extra);
    chunks.push(idat());
    chunks.push(iend());
    png(&chunks)
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// A zlib stream with one stored (uncompressed) deflate block; `data` is under 64 KiB.
pub fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let n = u16::try_from(data.len()).unwrap();
    let mut v = vec![0x78, 0x01, 0x01];
    v.extend_from_slice(&n.to_le_bytes());
    v.extend_from_slice(&(!n).to_le_bytes());
    v.extend_from_slice(data);
    v.extend_from_slice(&adler32(data).to_be_bytes());
    v
}

/// An iTXt chunk with the XMP keyword.
pub fn itxt_xmp(packet: &[u8], compressed: bool) -> Vec<u8> {
    let mut d = b"XML:com.adobe.xmp\0".to_vec();
    d.extend_from_slice(&[u8::from(compressed), 0, 0, 0]);
    if compressed {
        d.extend_from_slice(&zlib_stored(packet));
    } else {
        d.extend_from_slice(packet);
    }
    chunk(b"iTXt", &d)
}

pub fn iccp(profile: &[u8]) -> Vec<u8> {
    let mut d = b"icc\0\0".to_vec();
    d.extend_from_slice(&zlib_stored(profile));
    chunk(b"iCCP", &d)
}

/// ImageMagick raw profile text: `\n<name>\n<length>\n<hex lines>`.
pub fn raw_profile(name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut s = format!("\n{name}\n{:>8}\n", bytes.len());
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    for line in hex.as_bytes().chunks(72) {
        s.push_str(std::str::from_utf8(line).unwrap());
        s.push('\n');
    }
    s.into_bytes()
}

pub fn text_profile(keyword: &str, bytes: &[u8]) -> Vec<u8> {
    let mut d = keyword.as_bytes().to_vec();
    d.push(0);
    d.extend_from_slice(&raw_profile("exif", bytes));
    chunk(b"tEXt", &d)
}

pub fn ztxt_profile(keyword: &str, bytes: &[u8]) -> Vec<u8> {
    let mut d = keyword.as_bytes().to_vec();
    d.extend_from_slice(&[0, 0]);
    d.extend_from_slice(&zlib_stored(&raw_profile("exif", bytes)));
    chunk(b"zTXt", &d)
}

/// The chunks of a PNG file: type, data, and whether the CRC is right.
pub fn png_chunks(f: &[u8]) -> Vec<([u8; 4], Vec<u8>, bool)> {
    assert_eq!(&f[..8], &PNG_SIG);
    let mut i = 8;
    let mut out = Vec::new();
    while i + 12 <= f.len() {
        let len = u32::from_be_bytes(f[i..i + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = f[i + 4..i + 8].try_into().unwrap();
        let data = f[i + 8..i + 8 + len].to_vec();
        let crc = u32::from_be_bytes(f[i + 8 + len..i + 12 + len].try_into().unwrap());
        out.push((kind, data.clone(), crc == crc32(&[&kind[..], &data[..]])));
        i += 12 + len;
    }
    assert_eq!(i, f.len());
    out
}

// ---- WebP ----

/// A RIFF chunk with its pad byte.
pub fn riff_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = kind.to_vec();
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v.extend_from_slice(data);
    if data.len() % 2 == 1 {
        v.push(0);
    }
    v
}

pub fn webp(chunks: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = chunks.concat();
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&(body.len() as u32 + 4).to_le_bytes());
    v.extend_from_slice(b"WEBP");
    v.extend_from_slice(&body);
    v
}

pub fn vp8x(flags: u8, w: u32, h: u32) -> Vec<u8> {
    let mut d = vec![flags, 0, 0, 0];
    d.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
    d.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
    riff_chunk(b"VP8X", &d)
}

/// A lossy bitstream stub, 16 x 8, odd length (so it has a pad byte).
pub fn vp8() -> Vec<u8> {
    riff_chunk(
        b"VP8 ",
        &[0x10, 0x02, 0x00, 0x9D, 0x01, 0x2A, 16, 0, 8, 0, 1, 2, 3],
    )
}

/// A lossless bitstream stub, 16 x 8, no alpha.
pub fn vp8l() -> Vec<u8> {
    riff_chunk(b"VP8L", &[0x2F, 0x0F, 0xC0, 0x01, 0x00, 9, 9, 9])
}

/// The chunks of a WebP file: type and data; checks the RIFF size.
pub fn webp_chunks(f: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
    assert_eq!(&f[..4], b"RIFF");
    assert_eq!(&f[8..12], b"WEBP");
    assert_eq!(
        u32::from_le_bytes(f[4..8].try_into().unwrap()) as usize,
        f.len() - 8
    );
    let mut i = 12;
    let mut out = Vec::new();
    while i < f.len() {
        let kind: [u8; 4] = f[i..i + 4].try_into().unwrap();
        let len = u32::from_le_bytes(f[i + 4..i + 8].try_into().unwrap()) as usize;
        out.push((kind, f[i + 8..i + 8 + len].to_vec()));
        i += 8 + len + (len & 1);
    }
    assert_eq!(i, f.len());
    out
}

// ---- XMP packets ----

pub fn xmp_attr(orientation: &str) -> String {
    format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:tiff=\"http://ns.adobe.com/tiff/1.0/\" tiff:Orientation=\"{orientation}\"/></rdf:RDF></x:xmpmeta>"
    )
}

pub fn xmp_plain() -> String {
    "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\"/></rdf:RDF></x:xmpmeta>".to_string()
}
