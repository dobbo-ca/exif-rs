# Fuzzing

`tests/fuzz` is a cargo-fuzz crate (run with `--fuzz-dir tests/fuzz`); it is the only place that
depends on `libfuzzer-sys`. One target per entry point. Each runs with small `Limits` so one input
stays far below `-malloc_limit_mb`, and checks that the call returns (never panics, never hangs)
and that an error is one of the documented variants.

| Target | Entry point | Checks |
|---|---|---|
| `exif` | `Exif::parse` on arbitrary bytes | IFD walk terminates, cycles and bad offsets are skipped, every typed accessor is callable on the result |
| `xmp` | `Xmp::parse` (standard packet, and a split into packet + extended) | tolerant XML reader terminates, no panic on bad UTF-8, entities or nesting, accessors callable |
| `jpeg` | `extract` on bytes forced to start as a JPEG | segment walk, extended XMP and ICC reassembly |
| `png` | `extract` on bytes forced to start as a PNG | chunk walk, zTXt/iTXt/iCCP inflate under the budget, ImageMagick raw profile decode |
| `webp` | `extract` on bytes forced to start as a WebP | RIFF chunk walk, sizes and padding |
| `read` | `read` on arbitrary bytes | container, EXIF and XMP together under one budget |
| `strip` | `strip` and `reset_orientation` on arbitrary bytes | output is valid for the same container when `Ok`, image data bytes unchanged, no panic |

Every target uses a `max_alloc` far below `-malloc_limit_mb=512`; a peak above it is a bug in the
charging.

## CI

`.github/workflows/fuzz.yml` runs weekly, Sunday 12:23 UTC (`cron: '23 12 * * 0'`), and on
`workflow_dispatch`; never on push or pull request. Matrix over the seven targets with
`max-parallel: 2`, plus an arm64 matrix. Each target runs
`cargo +nightly fuzz run --fuzz-dir tests/fuzz <target> ... -- -max_total_time=$MAX_TOTAL_TIME
-rss_limit_mb=2048 -malloc_limit_mb=512`. Crash artifacts are uploaded.

Dispatch a 15-minute run per target:

```
gh workflow run fuzz.yml --ref <branch> -f max_total_time=900
```

The default is 600 s. The seed corpus is the parity corpus under `tests/data`.
