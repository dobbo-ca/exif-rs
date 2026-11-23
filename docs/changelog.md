# Changelog

## Unreleased

Strip keeps data by default, with an option to drop it (the owner's "Metadata rule" of
2026-11-19). This part covers JPEG and the `Keep` API; PNG and WebP follow.

- **Breaking:** `Keep::default()` now keeps everything that is not the metadata `strip` is for:
  the orientation, the ICC profile, unknown JPEG segments, the MPF images and the bytes after a
  JPEG's EOI. The v1 default dropped all of them, the ICC profile and the orientation included;
  `Keep::NONE` gives the v1 behaviour. `Keep` no longer derives `Default` (it implements it), and
  struct literals need the new fields (`Keep { orientation: true, ..Keep::NONE }`).
- New `Keep` fields: `unknown` (JPEG APPn segments that are not known metadata, kept as stored
  and in place), `mpf_images` (the MPF secondary images, such as an Ultra HDR gain map, and the
  MPF segment) and `trailing` (the bytes after the JPEG EOI: MPF images, a Motion Photo or Live
  Photo video, any other bytes). PNG and WebP ignore them for now.
- JPEG strip always drops the EXIF, XMP (standard, extended and the other headers ExifTool reads
  as XMP), APP13 Photoshop (`Photoshop 3.0\0`, `Adobe_Photoshop2.5:`) and COM segments. An APP1
  with `Exif\0` after up to 4 bytes, in any case, now counts as EXIF, as ExifTool reads it.
- JPEG data after the EOI is kept by `Keep::default()`, copied as it is, without the MPF images
  when `mpf_images` is off.
- MPF fix-up: `strip` sets the size of the primary image and the offsets of the images it moved
  in the first MPF segment; `reset_orientation` does the same when the XMP orientation value
  changes length.
- XMP fix-up: when bytes after the EOI are kept, `strip` writes a new XMP packet with only the
  fields that describe them (`hdrgm:Version`, the Motion Photo / MicroVideo fields,
  `Container:Directory`), without the dropped MPF images' items.
- Parity: a new strip mode `default` (`Keep::default()`) in the ExifTool checks, and Ultra HDR
  and Motion Photo files made in CI with libultrahdr's `ultrahdr_app` (CI-only), checked with
  ExifTool, Pillow and the libultrahdr decoder.
