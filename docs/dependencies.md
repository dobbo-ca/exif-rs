# Dependencies

## Crate dependencies

| Crate | Source | Why |
|---|---|---|
| `deflate` | `dobbo-ca/deflate-rs`, git, pinned by rev (owner crate) | zlib inflate for PNG `zTXt`, compressed `iTXt` (XMP) and `iCCP`, called as `deflate::zlib::decode_limit` under the remaining `max_alloc` budget. Writing an inflater here would duplicate a crate we own and fuzz. |

Nothing else: no C dependency, no `-sys` crate, no `cc`/`bindgen`, no proc macro, no XML crate (the
XMP reader is small and tolerant by design, see design.md). CI fetches the private git dependency
with a read-only GitHub App token (`repositories: deflate-rs`).

## Test-only

`libfuzzer-sys` lives only in `tests/fuzz` (the separate cargo-fuzz crate), never in the library.

## CI-only tools (never crate dependencies)

- ExifTool 13.64: the parity oracle. Read as a behaviour reference; no ExifTool code or table is
  copied (it is GPL/Artistic).
- Python 3 with Pillow: the strip pixel check, which decodes the original and the stripped file and
  compares the pixels.
- libultrahdr (github.com/google/libultrahdr, release v2.0.2, commit `e5f5a022`; Apache-2.0 and
  MIT): `ultrahdr_app`, built in its own stage of `.github/ci/Dockerfile` with
  `UHDR_WRITE_XMP=1` and copied into the parity stage. It makes the Ultra HDR test files and
  decodes the stripped ones (tests/parity/gainmap.py). No code of it is copied, so NOTICE does
  not list it.

None of these is needed to build or use the crate.
