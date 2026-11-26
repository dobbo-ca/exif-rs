# Benchmarks

There is no speed bar: the numbers are information only and never fail a job.

`.github/workflows/bench.yml` (`workflow_dispatch` and push to `main`, not required) builds the CI
image and, on the parity corpus in one job, times:

- ExifTool 13.64, wall time per file (process start included, which dominates for small files);
- the Rust bench example (`examples/bench.rs`, `std::time::Instant`, no framework), which runs
  `read` and `strip` over the same files with default `Limits`.

The job summary shows both and their ratio.

## Numbers

Main 450c284, `bench.yml` run 45716804597, on a GitHub-hosted `ubuntu-24.04` runner (the baseline for
later runs; information only).

| files | MB | read ms | strip ms |
|---|---|---|---|
| 2402 | 963.1 | 20.0 | 475.7 |

| | wall ms |
|---|---|
| ExifTool `-j -n -G1 -a -r` | 4377 |
| exif-rs `read` | 20.0 |
| exif-rs `strip` (`Keep::default()`) | 475.7 |
