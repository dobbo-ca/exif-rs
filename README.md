# exif-rs

Reads EXIF and XMP metadata, and strips or rewrites it, in JPEG, PNG and WebP files. Pure Rust, no
C dependencies, `#![forbid(unsafe_code)]`, bounded on hostile input. HEIF and AVIF come later
through isobmff-rs, which hands the exif and xmp bytes to `Exif::parse` and `Xmp::parse`.
ExifTool 13.64 is the parity oracle in CI.

```toml
exif = { git = "https://github.com/dobbo-ca/exif-rs", rev = "<sha>" }
```

## Quick start

```rust
use std::sync::atomic::AtomicBool;
use exif::{Keep, Limits};

let limits = Limits { max_threads: 1, max_alloc: 64 << 20, ..Limits::default() };
let cancel = AtomicBool::new(false);
let meta = exif::read(file, &limits, &cancel)?;
if let Some(exif) = &meta.exif {
    println!("orientation {:?}, taken {:?}", exif.orientation(), exif.capture_time());
}
// Keep::default() drops the EXIF, XMP and IPTC metadata and keeps the rest: the orientation,
// the ICC profile, unknown segments and the data after a JPEG's image (an Ultra HDR gain map, a
// Motion Photo video), with the MPF offsets fixed. Keep::NONE drops all of it (the v1 default).
let clean = exif::strip(file, Keep::default(), &limits, &cancel)?;
let bare = exif::strip(file, Keep { orientation: true, ..Keep::NONE }, &limits, &cancel)?;
```

## Resource limits

Every call takes `&Limits` and a cancel flag (`&AtomicBool`); there is no unlimited entry point.
Details: [docs/limits.md](docs/limits.md).

| Field | Default | Unit | Error (`Limit::...`) |
|---|---|---|---|
| `max_alloc` | 256 MiB | bytes, enforced peak | `Alloc` |
| `max_threads` | `available_parallelism` | threads | none |
| `max_exif_bytes` | 16 MiB | bytes | `ExifBytes` |
| `max_xmp_bytes` | 16 MiB | bytes (standard + extended) | `XmpBytes` |
| `max_ifds` | 64 | IFDs per EXIF block | `Ifds` |
| `max_entries` | 4096 | entries per IFD | `Entries` |
| `max_total_entries` | 65536 | entries, all IFDs | `TotalEntries` |
| `max_depth` | 8 | IFD nesting | `Depth` |
| `max_value_bytes` | 64 MiB | bytes, all values | `ValueBytes` |
| `max_segments` | 100 000 | JPEG segments / PNG or RIFF chunks | `Segments` |
| `max_xmp_nodes` | 1 000 000 | XML elements and attributes | `XmpNodes` |

- Each limit returns `Error::LimitExceeded(Limit::...)`.
- `max_alloc` is the enforced peak, including the zlib inflate of deflate-rs (PNG `zTXt`, `iTXt`,
  `iCCP`), which runs under the remaining budget: every buffer sized from the input is charged
  before it is allocated and released when a temporary is dropped.
- This crate never spawns a thread. `max_threads = 1` and every other value run on the calling
  thread, so a caller's pool is never fought.
- Set the cancel flag from another thread: the call returns `Error::Cancelled` (checked per IFD,
  per segment or chunk, and every 1024 XML nodes).

## Docs

[design](docs/design.md), [parity](docs/parity.md) (ExifTool 13.64 oracle; no speed bar, the bench
is information only), [limits](docs/limits.md), [threading](docs/threading.md),
[safety](docs/safety.md), [fuzzing](docs/fuzzing.md), [benchmarks](docs/benchmarks.md),
[dependencies](docs/dependencies.md), [testing](docs/testing.md), [changelog](docs/changelog.md).
