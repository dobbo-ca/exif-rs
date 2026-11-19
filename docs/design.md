# Design

exif-rs reads EXIF and XMP metadata and strips or rewrites it in JPEG, PNG and WebP files. It is
a new crate (no Go original). ExifTool 13.64 is the CI oracle (docs/parity.md).

```
 file bytes ──► container layer ──► Raw { exif, xmp, xmp_extended, icc }
 (JPEG/PNG/WebP)  src/container/          │
                                          ├─► Exif::parse  (src/tiff.rs, src/exif.rs)
 HEIF/AVIF later: isobmff-rs gives        └─► Xmp::parse   (src/xmp.rs)
 (exif bytes, xmp bytes) ─────────────────────▲
 file bytes ──► strip / reset_orientation (src/rewrite.rs + src/container/*) ──► new file bytes
```

## Rules for all code

- `#![forbid(unsafe_code)]`. No SIMD (no hot kernel here; docs/safety.md). No threads.
- Never panic on any input: no `unwrap`/`expect`/`panic!`/indexing that can go out of bounds
  (use `get`, `split_at_checked`, `chunks_exact`). Size arithmetic from input uses `checked_*`.
- Every buffer whose size comes from the input is allocated through `crate::budget::Budget`
  (`budget.vec(n)`, `budget.copy(src)`), never `vec![0; n]` or an unreserved `push` loop that
  grows from input sizes. Reserve the exact final size, then push.
- Check `budget.check_cancel()?` once per IFD, per JPEG segment / PNG chunk / RIFF chunk, and every
  1024 XML nodes.
- Never trust an offset or a length: bounds-check against the slice. A bad offset inside an IFD
  skips that entry or that IFD (ExifTool warns and goes on); it is not an error. Only a limit,
  cancel or OOM is a hard error inside a block; a block whose header is invalid is
  `Error::Malformed`.
- Public functions have rustdoc with `# Errors` naming the limits they enforce.

## Public API (src/lib.rs re-exports all of it)

```rust
// src/error.rs, src/limits.rs (written): Error, Limit, Limits.

// src/lib.rs
pub struct Metadata { pub raw: Raw, pub exif: Option<Exif>, pub xmp: Option<Xmp> }
pub fn read(data: &[u8], limits: &Limits, cancel: &AtomicBool) -> Result<Metadata, Error>;
// extract(), then Exif::parse / Xmp::parse on what it found. A Malformed/Truncated/UnknownFormat
// error from Exif::parse or Xmp::parse gives None for that part (the raw bytes stay in `raw`);
// LimitExceeded, Cancelled and OutOfMemory are returned. One Budget for the whole call.

// src/container/mod.rs
pub enum Container { Jpeg, Png, Webp }
pub fn detect(data: &[u8]) -> Option<Container>;           // by magic bytes only
pub struct Raw {
    pub container: Container,
    pub exif: Option<Vec<u8>>,          // starts at the TIFF header ("Exif\0\0" removed)
    pub xmp: Option<Vec<u8>>,           // the standard packet, as stored
    pub xmp_extended: Option<Vec<u8>>,  // JPEG extended XMP, chunks put together; else None
    pub icc: Option<Vec<u8>>,           // ICC profile (JPEG APP2 chunks joined, PNG iCCP inflated, WebP ICCP)
}
pub fn extract(data: &[u8], limits: &Limits, cancel: &AtomicBool) -> Result<Raw, Error>;
pub(crate) fn extract_with(data: &[u8], limits: &Limits, budget: &Budget) -> Result<Raw, Error>;

// src/exif.rs (types and typed accessors) + src/tiff.rs (the IFD walk)
pub enum ByteOrder { Little, Big }
pub enum IfdKind { Ifd0, Exif, Gps, Interop, Ifd1 }
pub struct Rational { pub num: u32, pub den: u32 }
pub struct SRational { pub num: i32, pub den: i32 }
pub enum Value {
    Byte(Vec<u8>), Ascii(Vec<u8>), Short(Vec<u16>), Long(Vec<u32>), Rational(Vec<Rational>),
    SByte(Vec<i8>), Undefined(Vec<u8>), SShort(Vec<i16>), SLong(Vec<i32>),
    SRational(Vec<SRational>), Float(Vec<f32>), Double(Vec<f64>),
    // type 13 (IFD) is read as Long; an unknown type is skipped
}
pub struct Entry { pub tag: u16, pub value: Value }
pub struct Exif { /* byte order, entries per IfdKind in file order, thumbnail */ }
impl Exif {
    pub fn parse(tiff: &[u8], limits: &Limits, cancel: &AtomicBool) -> Result<Exif, Error>;
    pub(crate) fn parse_with(tiff: &[u8], limits: &Limits, budget: &Budget) -> Result<Exif, Error>;
    pub fn byte_order(&self) -> ByteOrder;
    pub fn entries(&self, ifd: IfdKind) -> &[Entry];       // file order, duplicates kept
    pub fn get(&self, ifd: IfdKind, tag: u16) -> Option<&Value>; // first entry with that tag
    pub fn orientation(&self) -> Option<u16>;              // IFD0 0x0112, the stored value
    pub fn make(&self) -> Option<&str>;                    // IFD0 0x010F
    pub fn model(&self) -> Option<&str>;                   // IFD0 0x0110
    pub fn lens_make(&self) -> Option<&str>;               // Exif 0xA433
    pub fn lens_model(&self) -> Option<&str>;              // Exif 0xA434
    pub fn capture_time(&self) -> Option<CaptureTime>;     // needs DateTimeOriginal
    pub fn exposure(&self) -> Exposure;
    pub fn pixel_size(&self) -> Option<(u32, u32)>;        // Exif 0xA002, 0xA003 (both present)
    pub fn gps(&self) -> Option<Gps>;                      // None if the GPS IFD is absent
    pub fn maker_note(&self) -> Option<&[u8]>;             // Exif 0x927C, raw bytes, never parsed
    pub fn thumbnail(&self) -> Option<&[u8]>;              // IFD1 0x0201 offset + 0x0202 length
}
pub struct CaptureTime {
    pub date_time: String,            // Exif 0x9003 DateTimeOriginal, ASCII up to the first NUL
    pub subsec: Option<String>,       // Exif 0x9291 SubSecTimeOriginal
    pub offset: Option<String>,       // Exif 0x9011 OffsetTimeOriginal
}
pub struct Exposure {
    pub exposure_time: Option<Rational>,  // Exif 0x829A
    pub f_number: Option<Rational>,       // Exif 0x829D
    pub iso: Option<u32>,                 // Exif 0x8827 (first value)
    pub focal_length: Option<Rational>,   // Exif 0x920A
}
pub struct Gps {
    pub latitude: Option<[Rational; 3]>,  pub latitude_ref: Option<u8>,   // 0x0002, 0x0001 ('N'/'S')
    pub longitude: Option<[Rational; 3]>, pub longitude_ref: Option<u8>,  // 0x0004, 0x0003 ('E'/'W')
    pub altitude: Option<Rational>,       pub altitude_ref: Option<u8>,   // 0x0006, 0x0005 (0 above, 1 below)
    pub time_stamp: Option<[Rational; 3]>,                                 // 0x0007 UTC h, m, s
    pub date_stamp: Option<String>,                                        // 0x001D "YYYY:MM:DD"
}
impl Gps {
    pub fn latitude_deg(&self) -> Option<f64>;   // signed: S is negative
    pub fn longitude_deg(&self) -> Option<f64>;  // signed: W is negative
    pub fn altitude_m(&self) -> Option<f64>;     // signed: ref 1 is negative
}
// crate-internal, used by src/rewrite.rs:
pub(crate) fn orientation_value_offset(tiff: &[u8]) -> Option<(usize, ByteOrder)>; // in src/tiff.rs
// the byte offset in `tiff` of the 2-byte inline value of IFD0 0x0112 (type SHORT, count 1)

// src/xmp.rs
pub struct Xmp { /* raw packets and the parsed simple properties */ }
pub struct Property { pub ns: String, pub name: String, pub value: String }
impl Xmp {
    pub fn parse(packet: &[u8], extended: Option<&[u8]>, limits: &Limits, cancel: &AtomicBool) -> Result<Xmp, Error>;
    pub(crate) fn parse_with(packet: &[u8], extended: Option<&[u8]>, limits: &Limits, budget: &Budget) -> Result<Xmp, Error>;
    pub fn raw(&self) -> &[u8];
    pub fn properties(&self) -> &[Property];        // simple values, document order, extended last
    pub fn get(&self, ns: &str, name: &str) -> Option<&str>;
    pub fn orientation(&self) -> Option<u16>;       // tiff:Orientation (http://ns.adobe.com/tiff/1.0/)
    pub fn create_date(&self) -> Option<&str>;      // xmp:CreateDate (http://ns.adobe.com/xap/1.0/), raw text
    pub fn rating(&self) -> Option<&str>;           // xmp:Rating, raw text
    pub fn hdrgm(&self) -> impl Iterator<Item = &Property>; // ns http://ns.adobe.com/hdr-gain-map/1.0/, raw text
}
pub(crate) fn orientation_value_span(packet: &[u8], limits: &Limits) -> Option<std::ops::Range<usize>>;
// the byte range in `packet` of the raw text of the tiff:Orientation value (attribute value
// without quotes, or element text)

// src/rewrite.rs
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Keep { pub orientation: bool, pub icc: bool }
pub fn strip(data: &[u8], keep: Keep, limits: &Limits, cancel: &AtomicBool) -> Result<Vec<u8>, Error>;
pub fn reset_orientation(data: &[u8], limits: &Limits, cancel: &AtomicBool) -> Result<Vec<u8>, Error>;
```

## XMP parsing

A small tolerant XML reader, no DTD, no external entities (none are ever fetched): elements,
attributes (`'` or `"`), namespace declarations with scope, the five named entities and numeric
character references, comments, processing instructions, CDATA. A property is simple when it is
an attribute of `rdf:Description` (other than `rdf:about` and `xmlns:*`) or a child element of
`rdf:Description` with text only. Structures and `rdf:Seq` / `rdf:Bag` / `rdf:Alt` arrays: each
`rdf:li` text becomes one property with the array's name (so an `rdf:Alt` title with one entry is
one property). Malformed XML: return what was parsed before the error; `Error::Malformed` only when
nothing was parsed and the packet does not start with `<` after optional whitespace and BOM.

## Containers

- **JPEG**: walk the segments from SOI to the first SOS. APP1 `Exif\0\0` (the first one) is EXIF;
  APP1 `http://ns.adobe.com/xap/1.0/\0` is the standard XMP; APP1
  `http://ns.adobe.com/xmp/extension/\0` + 32-byte GUID + u32 full length + u32 offset are the
  extended XMP chunks (only for the GUID named by `xmpNote:HasExtendedXMP` in the standard packet;
  put together by offset, bounded by `max_xmp_bytes`); APP2 `ICC_PROFILE\0` + seq + count are the
  ICC chunks (joined in seq order). A file that ends before SOS still gives what was read.
- **PNG**: signature, then chunks to IEND (CRC not checked on read). `eXIf` is EXIF (an
  `Exif\0\0` prefix is removed if present). `iTXt` with keyword `XML:com.adobe.xmp` is XMP
  (inflate when the compression flag is set). `tEXt` / `zTXt` / `iTXt` with keyword
  `Raw profile type exif` or `Raw profile type APP1` hold ImageMagick hex text
  (`\n<name>\n<spaces><decimal length>\n<hex lines>`): decode it; the bytes start with
  `Exif\0\0`, which is removed. `eXIf` wins over a raw profile. `iCCP` is ICC (inflate). Inflate
  with `deflate::zlib::decode_limit` under the remaining budget and `max_exif_bytes` /
  `max_xmp_bytes`.
- **WebP**: `RIFF` size `WEBP`, then chunks (FourCC, u32 LE size, data, pad byte when odd).
  `EXIF` (an `Exif\0\0` prefix is removed), `XMP `, `ICCP`.

## Strip and rewrite

`strip` removes all metadata. `Keep.orientation` keeps the EXIF IFD0 Orientation (only it: a new
minimal EXIF block in the source byte order: TIFF header, IFD0 with one SHORT entry 0x0112, next
IFD 0). `Keep.icc` keeps the ICC profile bytes as stored. The image data bytes are never changed.

- **JPEG**: before the first SOS keep SOI, APP0 `JFIF\0`, APP14 `Adobe`, APP2 `ICC_PROFILE` (only
  with `keep.icc`) and every non-APPn, non-COM marker segment (DQT, DHT, SOFn, DRI, ...). Drop all
  other APPn and COM. The minimal EXIF APP1 goes right after SOI, or after APP0 JFIF when that is
  the first segment. From the first SOS up to and including EOI the bytes are copied as they are
  (segments between scans included). Bytes after EOI (MPF images, trailers) are dropped. No EOI:
  copy to the end.
- **PNG**: keep IHDR, PLTE, IDAT, IEND, tRNS, cHRM, gAMA, sBIT, sRGB, cICP, mDCV, cLLI, bKGD, hIST,
  pHYs, sPLT, acTL, fcTL, fdAT, and iCCP with `keep.icc`, verbatim. Drop all other chunks. The
  minimal EXIF goes into a new `eXIf` chunk (CRC computed) right before the first IDAT.
- **WebP**: keep VP8X, VP8 , VP8L, ALPH, ANIM, ANMF, and ICCP with `keep.icc`. Drop the rest.
  Clear the VP8X flags of what was dropped (EXIF 0x08, XMP 0x04, ICC 0x20), set EXIF when the
  minimal EXIF chunk is added (at the end), and write the new RIFF size.

`reset_orientation` changes only orientation values to 1, every other byte stays: the EXIF IFD0
Orientation value in place (2 bytes; PNG `eXIf` gets a new CRC), and each XMP `tiff:Orientation`
value text replaced by `1` (the JPEG APP1 length, PNG chunk length and CRC, WebP chunk size, pad
and RIFF size are updated). No orientation anywhere: the output equals the input.
