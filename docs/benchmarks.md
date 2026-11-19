# Benchmarks

There is no speed bar (owner decision): the numbers are information only and never fail a job.

`.github/workflows/bench.yml` (`workflow_dispatch` and push to `main`, not required) builds the CI
image and, on the parity corpus in one job, times:

- ExifTool 13.64, wall time per file (process start included, which dominates for small files);
- the Rust bench example (`examples/bench.rs`, `std::time::Instant`, no framework), which runs
  `read` and `strip` over the same files with default `Limits`.

The job summary shows both and their ratio.

## Numbers

Not measured yet. The first `bench.yml` run on `main` fills this table.

| Input class | Files | ExifTool 13.64 (ms/file) | exif-rs read (ms/file) | exif-rs strip (ms/file) |
|---|---|---|---|---|
| (placeholder) | | | | |
