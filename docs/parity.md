# Parity with ExifTool

exif-rs is a new crate with no Go original. Its oracle is ExifTool 13.64, which runs only in CI.
Every field exif-rs exposes is compared as exact text with what `exiftool -j -n -G1 -a -b` prints,
and every strip mode is checked with ExifTool and with a pixel decode.

Counts: see the latest parity run summary

## The run

`tests/parity/run.sh <corpus> <oracle-binary> <report-dir>` (POSIX sh; needs perl, python3 and
Pillow, otherwise it exits 2):

1. `tests/parity/inputs.sh <corpus> /work/in` makes a flat dir of symlinks to the inputs (see
   Corpus). ExifTool and the oracle both follow symlinks. `PARITY_EVERY_N=<n>` keeps every n-th.
2. `perl <corpus>/exiftool/exiftool -j -n -G1 -a -b -ext '*' /work/in` gives `exiftool.json`.
   `-n` turns off print conversion (the value after ValueConv), `-G1` puts the family-1 group in
   each key (`IFD0:Make`), `-a` keeps duplicates, `-b` prints binary values (base64). `-ext '*'`
   is needed because a directory scan skips files with unknown extensions (`*.fuzzed`).
3. The same with `-EXIF -XMP -ICC_Profile` gives `exiftool-blocks.json`: those three blocks are
   protected tags that `-b` with `-j` prints only when they are asked for by name.
4. `oracle read /work/in oracle.jsonl` (examples/oracle.rs, default `Limits` with
   `max_threads = 1`) writes one JSON object per file with the same keys and value text, or
   `{"SourceFile":..., "Error": <Error::class()>}`.
5. `tests/parity/compare.py` compares them (Read parity).
6. `oracle strip all|keep|orient1 /work/in /work/strip/<mode>`, ExifTool on each output dir,
   then `tests/parity/strip_check.py` (Strip checks).
7. `<report-dir>/summary.md` holds the counts, `<report-dir>/status` is 0 (pass) or 1 (fail);
   the script itself exits 0. `/work` is deleted.

The oracle output is deterministic and the same on every platform (it formats numbers with Rust,
not the C library); the `parity-arm64` job diffs the x86_64 and aarch64 outputs.

## Corpus

Fetched by `tests/parity/fetch.sh` at pinned commits; nothing of it is copied into this repo.

- **ExifTool 13.64** (github.com/exiftool/exiftool, commit `3d7a0538`): the oracle, and from
  `t/images` the `*.jpg`, `*.png`, `*.webp` files (any case) and every other file that starts
  with JPEG, PNG or RIFF-WEBP magic bytes. Licence: same terms as Perl (GPL or Artistic). It is
  used as a program and as data only; no ExifTool code or table is copied. The rules below were
  written new from a reading of its behaviour.
- **metadata-extractor-images** (github.com/drewnoakes/metadata-extractor-images, commit
  `7b56b649`): every file under `jpg/`, `png/`, `webp/` (not the `metadata/` dirs, not `*.txt`)
  — camera and phone files of many makes — and `fuzzing/*.fuzzed`, hostile files of any format.
  Its README: "You are free to use these media files however you wish."

No personal photos are used. A link is named `<source>__<path with / and spaces as _>`.

## Read parity

`compare.py` loads ExifTool's JSON with numbers kept as their exact text (`parse_float`/`parse_int`
give a `str` subclass), so `1.50` and `1.5` differ, and a number never equals a string. For each
input and each compared key: in both sides with the same type and text is a match; in one side
only, or different, is a mismatch. An input that ExifTool does not read as JPEG, PNG or WEBP
(`File:FileType`) and that exif-rs refuses with `unknown-format` is skipped and counted. A
mismatch listed in `tests/parity/known-differences.tsv` (`file<TAB>key<TAB>reason`) is counted
as known; any other mismatch fails the run.

Compared keys and where the oracle takes them from:

| Key | exif-rs | ExifTool -n value |
|---|---|---|
| IFD0:Orientation, IFD0:Make, IFD0:Model | `Exif::entries(Ifd0)` | ReadValue; Make and Model RawConv trims trailing `\s` |
| ExifIFD:DateTimeOriginal, SubSecTimeOriginal, OffsetTimeOriginal, ExposureTime, FNumber, ISO, FocalLength, ExifImageWidth, ExifImageHeight, LensMake, LensModel | `entries(Exif)` | ReadValue; SubSecTimeOriginal ValueConv trims trailing spaces |
| GPS:GPSLatitudeRef, GPSLongitudeRef, GPSAltitudeRef, GPSAltitude | `entries(Gps)` | ReadValue |
| GPS:GPSLatitude, GPS:GPSLongitude | `entries(Gps)` | `ToDegrees` |
| GPS:GPSTimeStamp | `entries(Gps)` | `ConvertTimeStamp` |
| GPS:GPSDateStamp | `entries(Gps)` | read as `undef`, trailing NULs removed, `ExifDate` |
| Composite:GPSLatitude, Composite:GPSLongitude | `Gps::latitude_deg`, `longitude_deg` | ToDegrees, negated for `S`/`W` |
| Composite:GPSAltitude | `Gps::altitude_m` | the GPSAltitude text, or `-abs` of it when the ref is not 0 |
| Composite:GPSDateTime | the GPS keys above | `"<date> <time>Z"` |
| Composite:SubSecDateTimeOriginal | `Exif::capture_time` | sub-seconds and offset added (RawConv) |
| IFD1:ThumbnailImage | `Exif::thumbnail` | the bytes (binary) |
| XMP-tiff:Orientation, XMP-xmp:CreateDate, XMP-xmp:Rating | `Xmp::properties` | the text; CreateDate `ConvertXMPDate` |
| XMP-hdrgm:* | `Xmp::hdrgm` | the text; unknown names XMPAutoConv |
| EXIF:EXIF, XMP:XMP, ICC_Profile:ICC_Profile | `Metadata::raw` | the blocks (binary) |

### ExifTool rules the oracle reproduces

Value conversion (ExifTool.pm `ReadValue`, Exif.pm, GPS.pm, XMP.pm):

- Several values of one entry are joined with one space (`ISO` with two values is the string
  `"100 200"`).
- ASCII is cut at the first NUL. An UNDEFINED value of one byte is read as a number; longer
  UNDEFINED values are their bytes.
- A rational is `sprintf("%.10g", num/den)`; `x/0` is `inf`, `0/0` is `undef`. An exact integer
  quotient is a Perl integer, so `0/x` is `0`, never `-0`. Signed and unsigned alike.
- FLOAT and DOUBLE print as Perl prints a number: `%.15g`, `0` for either zero, `Inf`, `-Inf`,
  `NaN`.
- Make and Model: `s/\s+$//` (`\s` is space, tab, LF, CR, VT, FF). SubSecTimeOriginal:
  `s/ +$//`. Perl's `$` also matches before a final newline, which then stays.
- `GPS::ToDegrees`: `''` when the value holds the word `inf` or `undef`; else the first three
  numbers (`[+-]?(?=\d|\.\d)\d*(\.\d*)?([Ee][+-]\d+)?`) as `d + (m + s/60)/60`, printed as a Perl
  number.
- `GPS::ConvertTimeStamp`: `f = (h*60 + m)*60 + s` from the space-separated parts (Perl
  numification), then `h = int(f/3600)`, `m = int(f/60)` of the rest, seconds as `%012.9f`;
  `>= 60` gives `00` and a carry into minutes and hours, else trailing zeros and a bare `.` are
  removed; `%.2d:%.2d:<s>`. A part that is not a finite number is not reproduced (no key).
- GPSDateStamp has `Format => 'undef'`: the stored bytes whatever the type, `s/\0+$//`, then
  `Exif::ExifDate`: `s/(\d{4})[^\d]*(\d{2})[^\d]*(\d{2})$/$1:$2:$3/`.
- Composite:GPSLatitude/GPSLongitude (`Require` GPS:GPSLatitude and its ref): `Gps::latitude_deg`
  as a Perl number when it has a value; else (a coordinate that is not RATIONAL, or has an
  `inf`/`undef` part) ExifTool's ValueConv on the texts, `$ref =~ /^S/i ? -$deg : $deg` (`W` for
  longitude), where `$deg` is the GPS:GPSLatitude text: `''` from ToDegrees stays `''` (printed
  as a string), and Perl's `-''` and `-"0"` are `0`.
- Composite:GPSAltitude (`Desire` the altitude and its ref, needs the ref): `Gps::altitude_m`
  when it has a value, with ref 0 the GPS:GPSAltitude text (`%.10g`), else `-abs(value)` printed
  as a Perl number; else ExifTool's ValueConv on the texts: only when the altitude text is
  `IsFloat` (`^[+-]?(?=\d|\.\d)\d*(\.\d*)?([Ee][+-]\d+)?$`), a true ref (not `''` or `0`) gives
  `-abs`, else the text. The XMP GPSAltitude fallback of the same Composite is not reproduced.
  Composite:GPSDateTime is `"$date $time" . "Z"` of the two converted GPS keys.
- Composite:SubSecDateTimeOriginal (raw values): when SubSecTimeOriginal starts with digits,
  `.<digits>` goes after the first ` hh:mm:ss` not already followed by `.digit` (no such time: no
  sub-seconds); when OffsetTimeOriginal matches `^([-+])(\d{1,2}):(\d{2})` and the date has no
  `-` or `+`, `sprintf('%s%.2d:%.2d')` is appended. Neither: no key.
- ThumbnailImage is a Composite with the IFD1 group, built when the length is not 0. When the
  offset and length end past the EXIF block, ExifTool (`ExtractImage`) reads the bytes from the
  file; exif-rs `read` does the same for JPEG (Sony DCR-PC110, Sigma DP2 Quattro: the length
  runs 1-2 bytes into the next segment).
- A TIFF header with a wrong magic number (not 42) is read; ExifTool only warns.
- XMP: `tiff:Orientation` and `xmp:Rating` are the text. `xmp:CreateDate` (a `date`) goes through
  `ConvertXMPDate`: `^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}:\d{2})(:\d{2})?\s*(\S*)$` becomes
  `$1:$2:$3 $4$5$6`, else a value starting with four digits has every `-` changed to `:`.
  hdrgm: `OffsetSDR`, `OffsetHDR`, `GainMapMin`, `GainMapMax`, `Gamma` are lists (one item prints
  as the item, more as a JSON array); `Version`, `BaseRenditionIsHDR`, `HDRCapacityMin`,
  `HDRCapacityMax` are single values. An unknown hdrgm property gets the name with its first
  letter upper-cased and XMPAutoConv (`n/d` to the quotient, `inf`, `undef`; else the full
  date-time form of `ConvertXMPDate`), and is a list when it repeats.
- The EXIF block tag exists only for a block of more than 16 bytes. ExifTool processes maker
  notes in place in the block it later prints, and `MakerNotes::ProcessKodakPatch` writes the
  2-byte IFD entry count at note offset 10: for MakerNoteKodak8b (Make has `Kodak`, note
  `MM\0*\0\0\0\x08\0?\0\0`) and MakerNoteKodak11 (Make without `Kodak`, Model has `Kodak` or
  `PixPro`, note `II*\0\x08\0\0\0?\0\0\0`) that is a copy of note bytes 8-9 to 10-11. The oracle
  applies it to EXIF:EXIF (Kodak PIXPRO AZ521, SP360).
- PNG XMP and ICC (`Metadata::raw`) also come from `tEXt`/`zTXt` `XML:com.adobe.xmp` and
  ImageMagick `Raw profile type xmp`/`APP1`/`icc`/`icm` chunks; WebP XMP also from `XMP\0`
  (docs/design.md, Containers). XMP:XMP is the packet as stored, so a packet that is not UTF-8
  prints as base64.

Duplicates (ExifTool.pm `FoundTag`, the `exiftool` JSON writer): with `-j` one key is printed per
group and name, the priority one. A later tag of priority 1 replaces an earlier one, a priority-0
tag does not. So in one IFD the last entry wins, except IFD0 Orientation (priority 0, and IFD0 is
not the priority directory in these files) where the first wins; `tiff:Orientation` (table
priority 0) and `xmp:CreateDate` (priority 0) keep the first, `xmp:Rating` and the hdrgm
single values the last, `hdrgm:Version` (`Avoid`) the first.

JSON writing (the `exiftool` script, `EscapeJSON` and `ConvertBinary`):

- A value that matches `^-?(\d|[1-9]\d{1,14})(\.\d{1,16})?(e[-+]?\d{1,3})?$` (any case `e`) is an
  unquoted number; `true`/`false` in any case is an unquoted lower-case boolean; anything else
  is a string (so `"0.004"` is the number `0.004`, `"1e-05"` the number `1e-05`, `"inf"` a
  string).
- A string that is not UTF-8 (ExifTool's `IsUTF8`, which also refuses U+FFFE and U+FFFF) is
  `"base64:<data>"`. Else `"`, `\`, tab, LF and CR are escaped, NULs removed and the other control
  characters written `\uXXXX`.
- A binary value (thumbnail, blocks) is `base64:<data>` unless every byte is tab, LF, CR,
  printable ASCII or 0x80-0xF7 and the bytes are UTF-8; then it is the text itself (an XMP
  packet usually is). Base64 is the standard alphabet with `=` padding and no line breaks.

### Known exif-rs choices that can differ

These follow the exif-rs API on purpose; a mismatch they cause goes in `known-differences.tsv`
with the reason:

- `Gps::latitude_deg`/`longitude_deg` need RATIONAL[3] and finite parts, `Gps::altitude_m` a
  RATIONAL; ExifTool reads any type and prints `''` or `0` for `inf`/`undef`. The oracle then
  uses ExifTool's rule on the texts (above), so those files do not test these accessors.
- `Exif::capture_time` reads the Exif IFD only; ExifTool's Composite also takes an IFD0
  DateTimeOriginal.
- exif-rs reads the first EXIF block and XMP packet of a file; ExifTool reads all of them.

## Strip checks

`strip_check.py` reads ExifTool's JSON of each output and of its original. Every output of a mode
is checked; the files exif-rs refused are counted by error class (`<mode>.errors`).

- **all** (`strip(Keep::default())`): only these keys may remain: groups `ExifTool`, `System`,
  `JFIF`, `Adobe` (APP14), `PNG-pHYs`, `PNG-cICP`; `File` except `File:Comment` and
  `File:ExifByteOrder`; `Composite:ImageSize`, `Megapixels`, `Duration`; `PNG:` tags of the
  kept chunks (ImageWidth, ImageHeight, BitDepth, ColorType, Compression, Filter, Interlace,
  Palette, Transparency, WhitePointX/Y, RedX/Y, GreenX/Y, BlueX/Y, Gamma, SignificantBits,
  SRGBRendering, BackgroundColor, PaletteHistogram, SuggestedPalette, AnimationFrames,
  AnimationPlays); `RIFF:` tags of VP8, VP8L, VP8X, ANIM, ANMF and ALPH (VP8Version, ImageWidth,
  ImageHeight, HorizontalScale, VerticalScale, AlphaIsUsed, WebP_Flags, BackgroundColor,
  AnimationLoopCount, Duration, AlphaPreprocessing, AlphaFiltering, AlphaCompression). Anything
  else (IFD0, IFD1, ExifIFD, GPS, InteropIFD, MakerNotes, maker groups, XMP-*, IPTC, Photoshop,
  ICC*, PNG text, ...) is a failure.
- **keep** (`strip(Keep { orientation: true, icc: true })`): as `all`, plus `IFD0:Orientation`
  equal to the original's (absent when the original has none), `File:ExifByteOrder` only with
  it, and every `ICC*` key (and `PNG:ProfileName`, and `PNG:ICC_Profile`, ExifTool's tag for an
  ICC chunk it cannot decode) equal to the original's, none added or lost.
- **orient1** (`reset_orientation`): every key equal to the original's except the `System`
  group (file name, size, dates) and `IFD0:Orientation` and `XMP-tiff:Orientation`, which are
  `1` when the original has them and absent when it does not.

Pixel check (all modes): Pillow decodes the original and the output (`Image.open`, then for every
frame `seek`, `load`, and mode, size and a SHA-256 of `tobytes()`); they must be equal. For JPEG
only the first frame counts: Pillow opens a JPEG with an MPF segment as MPO, whose other frames
are the images after EOI, which strip drops by design. An
original that Pillow cannot decode (truncated, hostile, decompression bomb) is skipped and
counted by exception class. An output that Pillow cannot decode while its original decodes is a
failure.

## Reading the report

`summary.md`: the result, the ExifTool and corpus commits, then

- Read parity: inputs, inputs per container (magic bytes), inputs ExifTool reads as
  JPEG/PNG/WEBP, ExifTool file types, exif-rs error classes, skipped inputs, and per key the
  number of files compared, matched, mismatched and known. A key with 0 compared is not in the
  corpus.
- Strip checks: per mode the outputs, exif-rs errors, files with tag failures, pixel checks done,
  identical, failed and skipped (with reasons).

Lists: `read-mismatches.tsv` (file, key, ExifTool value, oracle value, as type and text),
`strip-failures.tsv` (mode, file, tags or pixels, detail), `strip-<mode>.errors`, and
`exiftool.stderr`.

## Known differences

None yet (`tests/parity/known-differences.tsv` is empty).
