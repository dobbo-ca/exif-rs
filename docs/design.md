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
    // type 13 (IFD) is read as Long, type 129 (EXIF 3.0 UTF-8) as Ascii; an unknown type is skipped
}
pub struct Entry { pub tag: u16, pub value: Value }
pub struct Exif { /* byte order, entries per IfdKind in file order, thumbnail */ }
impl Exif {
    pub fn parse(tiff: &[u8], limits: &Limits, cancel: &AtomicBool) -> Result<Exif, Error>;
    pub(crate) fn parse_with(tiff: &[u8], limits: &Limits, budget: &Budget) -> Result<Exif, Error>;
    pub fn byte_order(&self) -> ByteOrder;
    pub fn entries(&self, ifd: IfdKind) -> &[Entry];       // file order, duplicates kept
    pub fn get(&self, ifd: IfdKind, tag: u16) -> Option<&Value>; // first entry with that tag
    // the accessors below read the last entry of a tag (ExifTool keeps the last, priority 1),
    // except orientation (priority 0: the first) and thumbnail (the first 0x0201 and 0x0202)
    pub fn orientation(&self) -> Option<u16>;              // IFD0 0x0112, first value of the first entry; None unless an integer type (IFD as LONG)
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
    // (from `read` of a JPEG the range may end past the APP1 segment: read from the file)
}
pub struct CaptureTime {
    pub date_time: String,            // Exif 0x9003 DateTimeOriginal, ASCII up to the first NUL
    pub subsec: Option<String>,       // Exif 0x9291 SubSecTimeOriginal
    pub offset: Option<String>,       // Exif 0x9011 OffsetTimeOriginal
}
pub struct Exposure {
    // the Rational fields (here and in Gps) also take a non-negative SRATIONAL (signs dropped)
    // or an integer (n/1); iso and pixel_size any non-negative integer type: ExifTool reads any type
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
    pub fn altitude_m(&self) -> Option<f64>;     // signed: any ref but 0 is negative (ExifTool's Composite)
}
// crate-internal, used by src/rewrite.rs:
pub(crate) fn orientation_value_range(tiff: &[u8]) -> Option<(Range<usize>, ByteOrder)>; // in src/tiff.rs
// the bytes of the first value of IFD0 0x0112 (the first entry the walk keeps, inline or not; None unless an integer type)

// src/xmp.rs
pub struct Xmp { /* raw packets and the parsed simple properties */ }
pub struct Property { pub ns: String, pub name: String, pub value: String, pub top_level: bool }
impl Xmp {
    pub fn parse(packet: &[u8], extended: Option<&[u8]>, limits: &Limits, cancel: &AtomicBool) -> Result<Xmp, Error>;
    pub(crate) fn parse_with(packet: &[u8], extended: Option<&[u8]>, limits: &Limits, budget: &Budget) -> Result<Xmp, Error>;
    pub fn raw(&self) -> &[u8];
    pub fn properties(&self) -> &[Property];        // simple values, document order, extended last
    pub fn get(&self, ns: &str, name: &str) -> Option<&str>; // first top-level property
    pub fn orientation(&self) -> Option<u16>;       // first top-level tiff:Orientation (http://ns.adobe.com/tiff/1.0/)
    pub fn create_date(&self) -> Option<&str>;      // first top-level xmp:CreateDate (http://ns.adobe.com/xap/1.0/), raw text
    pub fn rating(&self) -> Option<&str>;           // last top-level xmp:Rating, raw text
    pub fn hdrgm(&self) -> impl Iterator<Item = &Property>; // ns http://ns.adobe.com/hdr-gain-map/1.0/, raw text
}
pub(crate) fn orientation_value_span(packet: &[u8], limits: &Limits, budget: &Budget) -> Result<Option<std::ops::Range<usize>>, Error>;
// the byte range in `packet` of the raw text of the first top-level tiff:Orientation value
// (attribute value without quotes, or element text), charged to the call's budget;
// max_xmp_bytes, max_xmp_nodes, max_alloc and cancel apply

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
one property). A property is top-level (`Property::top_level`) when it belongs to an
`rdf:Description` that is not inside another property, or is an item of such a property's array;
fields of structures (an `rdf:Description` or `rdf:parseType="Resource"` inside a property, such
as Photoshop's `xmpMM:Pantry` items) are listed by `properties()` but are not top-level, and
`get`, the typed accessors and `reset_orientation` look at top-level properties only (ExifTool
gives struct fields flattened names, so `tiff:Orientation` inside a Pantry item is
`XMP-xmpMM:PantryOrientation`, not `XMP-tiff:Orientation`). Malformed XML: return what was parsed
before the error; `Error::Malformed` only when nothing was parsed and the packet does not start
with `<` after optional whitespace and BOM.

## Containers

- **JPEG**: walk the segments from SOI to the first SOS. APP1 `Exif\0\0` (the first one) is EXIF;
  APP1 `http://ns.adobe.com/xap/1.0/\0` is the standard XMP; APP1
  `http://ns.adobe.com/xmp/extension/\0` + 32-byte GUID + u32 full length + u32 offset are the
  extended XMP chunks (only for the GUID named by `xmpNote:HasExtendedXMP` in the standard packet,
  compared exactly; a later chunk at the same offset replaces an earlier one; from offset 0 each
  chunk leads to the offset after it, and the packet is read only when that chain ends exactly at
  the full length of the last chunk, as ExifTool does; bounded by `max_xmp_bytes`); APP2
  `ICC_PROFILE\0` + number + total are the ICC chunks, collected as ExifTool does: the total of
  the first chunk, then chunks (numbered from 0 or 1; a repeated number is appended) until as many
  as the total were seen (a total of 0: the first chunk), joined in number order; a chunk with
  another total before that gives no profile. APP1 `Exif\0\0` segments right after the EXIF
  segment whose data has no TIFF header continue the block (ExifTool's multi-segment EXIF) and are
  appended to it. A file that ends before SOS still gives what was read.
- **PNG**: signature, then chunks to IEND (CRC not checked on read). `eXIf` is EXIF (an
  `Exif\0\0` prefix is removed if present). A `tEXt`, `zTXt` or `iTXt` (compression method 0)
  with keyword `XML:com.adobe.xmp` is XMP, as stored (inflated when compressed; as ExifTool, not
  only `iTXt`). Text chunks with keyword `Raw profile type <name>` hold ImageMagick hex text
  (`\n<name>\n<spaces><decimal length>\n<hex lines>`; fewer hex digits than the length: not a
  profile): `exif` and `APP1` give EXIF when the bytes start with `Exif\0\0` (removed) or a TIFF
  header, or XMP when they start with the APP1 XMP signature (removed); `xmp` gives XMP; `icc`
  and `icm` give ICC. The first source of each block wins, except that `eXIf` and `iCCP` win over
  a raw profile. `iCCP` is ICC (inflate). Inflate with `deflate::zlib::decode_limit` under the
  remaining budget and `max_exif_bytes` / `max_xmp_bytes`.
- **WebP**: `RIFF` size `WEBP`, then chunks (FourCC, u32 LE size, data, pad byte when odd).
  `EXIF` (an `Exif\0\0` prefix is removed), `XMP ` (or `XMP\0`, which some writers use and
  ExifTool reads), `ICCP`.

The TIFF header check takes only the byte order (`II` / `MM`) and an IFD0 offset of at least 8:
a wrong magic number (not 42) is accepted, as ExifTool does (it only warns). `read` of a JPEG
reads the IFD1 thumbnail from the file when its offset and length end past the APP1 segment
(some cameras count the segment header in the length; ExifTool reads such a thumbnail from the
file too); the bytes are bounded by the file and charged to the budget and `max_value_bytes`. Not
for a multi-segment EXIF block, whose offsets past the first segment do not match the file.

The IFD walk follows ExifTool's order: a sub-IFD is walked when its pointer entry is met (so the
Interop IFD inside the Exif IFD, and GPS before Exif when its entry comes first), IFD1 after all
of IFD0's sub-IFDs. When two pointers share an offset, the IFD walked first keeps it; a GPS
pointer to the Interop IFD's offset reads it again as GPS (ExifTool's patch for a Windows Phone
bug).

## Strip and rewrite

`strip` removes all metadata. `Keep.orientation` keeps the EXIF IFD0 Orientation (only it: a new
minimal EXIF block in the source byte order: TIFF header, IFD0 with one SHORT entry 0x0112, next
IFD 0). `Keep.icc` keeps the ICC profile bytes as stored. The image data bytes are never changed.

- **JPEG**: before the first SOS keep SOI, APP0 `JFIF\0`, APP14 `Adobe`, APP2 `ICC_PROFILE` (only
  with `keep.icc`) and every non-APPn, non-COM marker segment (DQT, DHT, SOFn, DRI, ...). An APP0
  JFIF segment is kept byte for byte, with the uncompressed RGB thumbnail JFIF 1.02 allows in it,
  if there is one (image data, not metadata, which ExifTool does not report either). Drop all
  other APPn and COM. The minimal EXIF APP1 goes right after SOI, or after APP0 JFIF when that is
  the first segment. From the first SOS up to and including EOI the bytes are copied as they are
  (segments between scans included). Bytes after EOI (MPF images, trailers) are dropped. No EOI:
  copy to the end.
- **PNG**: keep IHDR, PLTE, IDAT, IEND, tRNS, cHRM, gAMA, sBIT, sRGB, cICP, mDCV, cLLI, bKGD, hIST,
  pHYs, sPLT, acTL, fcTL, fdAT, and with `keep.icc` iCCP and the `Raw profile type icc` / `icm`
  text chunks, verbatim. Drop all other chunks. The
  minimal EXIF goes into a new `eXIf` chunk (CRC computed) right before the first IDAT.
- **WebP**: keep VP8X, VP8 , VP8L, ALPH, ANIM, ANMF, and ICCP with `keep.icc`. Drop the rest.
  Clear the VP8X flags of what was dropped (EXIF 0x08, XMP 0x04, ICC 0x20), set EXIF when the
  minimal EXIF chunk is added (at the end), and write the new RIFF size.

`reset_orientation` changes only orientation values to 1, every other byte stays: the first value
of the EXIF IFD0 Orientation in place (the first entry; none when it is not an integer type), in
the width of its type (BYTE, SHORT, LONG, IFD or signed; a JPEG block continued in the next APP1
segments is read joined and the value written back to the segment that holds it;
PNG `eXIf` gets a new CRC), and the text of the first top-level XMP `tiff:Orientation` (the one
ExifTool reports) replaced by `1` (the JPEG APP1 length, PNG chunk length and CRC, WebP chunk
size, pad and RIFF size are updated). A chunk none of whose bytes change keeps its CRC as stored.
No orientation anywhere: the output equals the input. The XMP scan is charged to the call's
budget, checks its cancel flag, and its limits are errors as in `read`. Not rewritten:
compressed XMP and the hex text of PNG raw profiles (EXIF or XMP).
