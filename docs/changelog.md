# Changelog

## Unreleased

Strip keeps data by default, with an option to drop it (the owner's "Metadata rule" of
2026-11-19), in JPEG, PNG and WebP.

- **Breaking:** `Keep::default()` now keeps everything that is not the metadata `strip` is for:
  the orientation, the ICC profile, unknown JPEG segments, unknown PNG and WebP chunks, the MPF
  images and the bytes after the image (after a JPEG's EOI, a PNG's IEND, the end a WebP's RIFF
  size gives). The v1 default dropped all of them, the ICC profile
  and the orientation included; `Keep::NONE` gives the v1 behaviour in every container. `Keep` no
  longer derives `Default` (it implements it), and struct literals need the new fields
  (`Keep { orientation: true, ..Keep::NONE }`).
- New `Keep` fields: `unknown` (JPEG APPn segments, PNG chunks and WebP chunks that are not known
  metadata or image data, kept as stored and in place), `mpf_images` (the MPF secondary images,
  such as an Ultra HDR gain map, and the MPF segment) and `trailing` (the bytes after the image,
  as stored and, but for the WebP metadata chunks, not parsed: after the JPEG EOI MPF images, a Motion Photo or Live Photo video,
  any other bytes; after the PNG IEND; after the end the WebP RIFF size gives and its pad byte).
  `mpf_images` is JPEG only.
- JPEG strip always drops the EXIF, XMP (standard, extended and the other headers ExifTool reads
  as XMP), APP13 Photoshop (`Photoshop 3.0\0`, `Adobe_Photoshop2.5:`) and COM segments. An APP1
  with `Exif\0` after up to 4 bytes, in any case, now counts as EXIF, as ExifTool reads it.
- JPEG data after the EOI is kept by `Keep::default()`, copied as it is, without the MPF images
  when `mpf_images` is off.
- PNG strip always drops `eXIf` (and the once-proposed `zxIf` / `zXIf`), `tIME`, `tEXt`, `zTXt`
  and `iTXt` (but the `Raw profile type icc` / `icm` text chunks, kept with `icc` as before). With
  `unknown` (on in `Keep::default()`) every other chunk the v1 list did not keep stays, as stored
  and in place (a private chunk, C2PA `caBX`, a gain map chunk, `oFFs`, `vpAg`, ...). With
  `trailing` (on in `Keep::default()`) the bytes after IEND stay; v1 dropped them. The new `eXIf`
  goes right before the first IDAT, or before Apple's `iDOT` when that comes first (iDOT holds
  the offset of the first IDAT from itself).
- WebP strip always drops `EXIF`, `XMP ` and `XMP\0`, and (as v1 did) a VP8X that is not the
  first one or is shorter than 10 bytes. With `unknown` (on in `Keep::default()`) every other
  chunk the v1 list did not keep stays, as stored and in place with its pad byte; the VP8X flags
  and the RIFF size follow. With `trailing` (on in `Keep::default()`) the bytes after the end the
  RIFF size gives (and its pad byte) stay, after the new RIFF data, but for the `EXIF`, `XMP `
  and `XMP\0` chunks among them, which ExifTool reads there too; v1 dropped them.
  A VP8X chunk added for the orientation now goes first
  (before any other chunk), not right before the VP8/VP8L chunk.
- MPF fix-up: `strip` sets the size of the primary image and the offsets of the images it moved
  in the first MPF segment; `reset_orientation` does the same when the XMP orientation value
  changes length.
- XMP fix-up: when bytes after the EOI are kept, `strip` writes a new XMP packet with only the
  fields that describe them (`hdrgm:Version`, the Motion Photo / MicroVideo fields,
  `Container:Directory`), without the dropped MPF images' items.
- Parity: a new strip mode `default` (`Keep::default()`) in the ExifTool checks, and Ultra HDR
  and Motion Photo files made in CI with libultrahdr's `ultrahdr_app` (CI-only), checked with
  ExifTool, Pillow and the libultrahdr decoder. Mode default checks that no metadata chunk is
  left in a PNG or WebP output.
- `fuzz.yml`: a `targets` dispatch input runs a subset of the fuzz targets.
