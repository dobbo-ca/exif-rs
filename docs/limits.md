# Limits

`Limits` (src/limits.rs) has public fields and a finite `Default`. Every public entry point takes
`&Limits` and `&AtomicBool`. A limit gives `Error::LimitExceeded(Limit::X)`; its class token (the
parity oracle writes it) is `limit-<field>`.

| Field | Default | Unit | `Limit` | Class token | Checked |
|---|---|---|---|---|---|
| `max_alloc` | 256 MiB | bytes, peak | `Alloc` | `limit-alloc` | `Budget::charge`, before each allocation sized from input |
| `max_threads` | `available_parallelism()` (1 if unknown) | threads | none | none | never fails; see threading.md |
| `max_exif_bytes` | 16 MiB (`16 << 20`) | bytes | `ExifBytes` | `limit-exif-bytes` | EXIF block size, in the container layer and in `Exif::parse` |
| `max_xmp_bytes` | 16 MiB | bytes, standard + extended | `XmpBytes` | `limit-xmp-bytes` | XMP packet size (extended chunks put together, PNG inflate cap), in the container layer and in `Xmp::parse` |
| `max_ifds` | 64 | IFDs, all kinds | `Ifds` | `limit-ifds` | IFD walk, before each IFD is read |
| `max_entries` | 4096 | entries in one IFD | `Entries` | `limit-entries` | IFD walk, from the entry count of the IFD, before its entries are read |
| `max_total_entries` | 65536 | entries, all IFDs | `TotalEntries` | `limit-total-entries` | IFD walk, running total |
| `max_depth` | 8 | nesting (IFD0 = 0, Exif = 1, Interop = 2) | `Depth` | `limit-depth` | IFD walk, when following a sub-IFD pointer |
| `max_value_bytes` | 64 MiB | bytes, all values | `ValueBytes` | `limit-value-bytes` | IFD walk, running total of value bytes, before a value is read |
| `max_segments` | 100 000 | JPEG segments, PNG chunks or RIFF chunks | `Segments` | `limit-segments` | container walk, per segment or chunk |
| `max_xmp_nodes` | 1 000 000 | XML elements + attributes | `XmpNodes` | `limit-xmp-nodes` | XMP parser, per node |

The other errors are not limits: `UnknownFormat`, `Truncated`, `Malformed(token)`, `OutOfMemory`
(the allocator refused a size that is within the limits), `Cancelled`.

## max_alloc and the budget

`max_alloc` is the peak working memory of one call, not a total. `src/budget.rs` keeps one ledger
per public call (`read` makes one `Budget` for the container, EXIF and XMP stages together):

- `Budget::charge(n)` adds `n` to the bytes in use, or fails with `LimitExceeded(Alloc)` and
  charges nothing. The charge comes before the allocation, so an input cannot make the crate
  allocate and then notice. The sum uses `checked_add`.
- `Budget::vec(n)` charges `n * size_of::<T>()` (checked), then `try_reserve_exact(n)`. If the
  allocator refuses, the charge is given back and the error is `OutOfMemory`.
- `Budget::copy(src)` is `vec(src.len())` plus the copy.
- `Budget::release(n)` gives bytes back when a temporary buffer is dropped (an inflated PNG text
  chunk once it is parsed, a joined extended-XMP buffer). Results that the caller keeps stay
  charged until the call returns.
- `Budget::remaining()` is passed down to nested calls: PNG inflate runs with
  `deflate::zlib::decode_limit` under the remaining budget, `max_exif_bytes` and `max_xmp_bytes`.

What is charged: copies of the EXIF, XMP and ICC blocks, parsed entry lists and values, XMP
property strings, inflated PNG text, the output buffer of `strip` and `reset_orientation`. What is
not: small fixed-size allocations that do not scale with the input. `tests/resources.rs` measures
the real peak with a counting allocator and asserts it stays within `max_alloc` plus a fixed slack.

## Cancel

`Budget::check_cancel` is called once per IFD, per JPEG segment, PNG chunk or RIFF chunk, and every
1024 XML nodes. A set flag gives `Error::Cancelled`.
