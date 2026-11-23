# Testing

All builds and tests run in CI (nothing runs on the owner's Mac).

## Test files

| File | Covers |
|---|---|
| `tests/exif.rs` | `Exif::parse`: both byte orders, value types, typed accessors, GPS, thumbnail, bad offsets, cycles, truncated IFDs |
| `tests/xmp.rs` | `Xmp::parse`: attributes and elements, `rdf:Seq/Bag/Alt`, entities, malformed XML, extended XMP |
| `tests/jpeg.rs` | JPEG container: APP1 EXIF/XMP, extended XMP, ICC chunks, truncated files |
| `tests/png.rs` | PNG container: `eXIf`, `iTXt`/`zTXt`/`tEXt`, ImageMagick raw profiles, `iCCP`, inflate limits |
| `tests/webp.rs` | WebP container: `EXIF`, `XMP `, `ICCP`, odd chunk padding, VP8X flags |
| `tests/strip.rs` | `strip`, `Keep`, `reset_orientation` in all three containers; image data bytes unchanged |
| `tests/trailing.rs` | JPEG data after the EOI: Ultra HDR (MPF in both byte orders and places), Motion Photo, both; `Keep::default()`, `trailing` and `mpf_images` off, `Keep::NONE` equal to v1; MPF and directory fix-up in `strip` and `reset_orientation`; unknown APPn; hostile MPF counts and offsets, absurd directory lengths, every cut |
| `tests/resources.rs` | counting global allocator: peak within `max_alloc` + slack for `read` and `strip` at 64 KiB, 1 MiB and default; `LimitExceeded(Alloc)` when the data is larger; same result for `max_threads` 1 and 4; no thread spawn in `src/` |
| `tests/limits_api.rs` | `Limits::default()` values, `Error::class` tokens, every `Limit` variant reached through the public API, cancel |

Unit tests in `src/container/mpf.rs`: the MPF fix-up in both byte orders, images before and after
the shift, a bad count, an entry out of range, cut blocks, the offset map.

Doc-tests (the crate-level example in `src/lib.rs`) are compiled by `cargo test`.

## CI jobs (`.github/workflows/ci.yml`)

- `test`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, and the
  aarch64 release build. Runs on pull requests; the only required check.
- `arm64`: `cargo test` for aarch64 under `qemu-aarch64`. The crate has no SIMD, so this checks the
  byte-order and arithmetic paths on a second target. Push to `main` and dispatch only.
- `parity`: ExifTool 13.64 against `examples/oracle.rs` on the same inputs (docs/parity.md).
- `parity-arm64`: the oracle example under qemu on a subset of the inputs.
- `fuzz` (`fuzz.yml`): weekly and dispatch, docs/fuzzing.md.
- `bench` (`bench.yml`): information only, docs/benchmarks.md.

The crate is done when `arm64`, `parity` and `parity-arm64` are green on `main`.
