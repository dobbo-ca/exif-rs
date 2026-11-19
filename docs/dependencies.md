# Dependencies

## Crate dependencies

| Crate | Source | Why |
|---|---|---|
| `deflate` | `dobbo-ca/deflate-rs`, git, pinned by rev (owner crate) | zlib inflate for PNG `zTXt`, compressed `iTXt` (XMP) and `iCCP`, called as `deflate::zlib::decode_limit` under the remaining `max_alloc` budget. Writing an inflater here would duplicate a crate we own and fuzz. |

Nothing else: no C dependency, no `-sys` crate, no `cc`/`bindgen`, no proc macro, no XML crate (the
XMP reader is small and tolerant by design, see design.md). CI fetches the private git dependency
with the `dobbo-ca-ci-read` GitHub App token (`repositories: deflate-rs`), as in conventions.

## Test-only

`libfuzzer-sys` lives only in `tests/fuzz` (the separate cargo-fuzz crate), never in the library.

## CI-only tools (never crate dependencies)

- ExifTool 13.64: the parity oracle. Read as a behaviour reference; no ExifTool code or table is
  copied (it is GPL/Artistic).
- Python 3 with Pillow: the strip pixel check, which decodes the original and the stripped file and
  compares the pixels.

None of these is needed to build or use the crate.
