# Safety

- `#![forbid(unsafe_code)]` at the crate root. There is no `unsafe` anywhere in the crate. (The
  counting allocator in tests/resources.rs needs `unsafe impl GlobalAlloc`; integration tests are
  separate crates, so the `forbid` does not reach them.)
- No SIMD tier, so no `target_feature` code, no runtime detection, no `<CRATE>_SIMD` override, no
  scalar/SIMD parity tests. A SIMD tier is for crates with hot kernels. This crate
  has none: the work is walking a few hundred IFD entries, a tolerant XML pass over a few KiB, and
  copying bytes. There is no speed bar, the bench is information only
  (benchmarks.md), and a kernel would only add `unsafe` surface.
- Non-test code denies `clippy::unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`
  (`cfg_attr(not(test), deny(...))` in src/lib.rs). CI runs clippy with `-D warnings` on all
  targets.

## Hostile input

Every input is untrusted: file bytes, EXIF offsets, XML.

- Bounded IFD walk: `max_ifds`, `max_entries`, `max_total_entries`, `max_depth`,
  `max_value_bytes`, checked before an IFD or value is read.
- Cycle detection: an IFD offset that was already visited is not followed again, so a pointer loop
  ends instead of consuming the IFD limit slowly.
- Checked arithmetic: every size, offset and count taken from the input uses `checked_*` or
  `get`/`split_at_checked`/`chunks_exact`; a failed check skips the entry (a bad offset inside an
  IFD is not an error, as in ExifTool) or returns `Malformed`/`Truncated` for a bad container.
- Budgeted allocation: buffers sized from the input are charged against `max_alloc` before they
  are allocated (limits.md); `try_reserve_exact`, never `vec![0; n]` on an input size.
- Bounded containers and XML: `max_segments`, `max_xmp_nodes`, inflate under the remaining budget
  and `max_exif_bytes` / `max_xmp_bytes`. XMP has no DTD and fetches no external entity.
- Cancel flag checked on every loop that can run long.
- No panics: fuzz targets (fuzzing.md) run with `-malloc_limit_mb=512`, so an allocation outside
  the limits is a crash too.
