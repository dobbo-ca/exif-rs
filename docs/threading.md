# Threading

This crate has no threads. It never calls `std::thread::spawn`, `thread::scope` or a thread
builder, and it has no pool. Every call runs on the calling thread.

Why: metadata parsing is small and per file. An EXIF block is usually under 64 KiB, an XMP packet
a few KiB; the walk is a pointer-chasing loop over a few hundred entries, and a container walk
reads segment headers and skips the image data. There is no tile, strip, band or plane to split,
so a thread would cost more than the work. Callers that read many files parallelise across files
themselves.

`Limits::max_threads` exists so that a caller sets the same fields on every dobbo-ca crate. It is
honoured trivially: the call uses one thread (the calling thread), which is within every value of
`max_threads` including 0 and 1. The default is `available_parallelism()` and changes nothing.
The result of a call is the same for every `max_threads`.

Cancellation from another thread works: `read`, `strip` and the others take `&AtomicBool`, and the
call reads it with relaxed loads at the points listed in limits.md. `Budget` is not `Sync`, by
design: one budget belongs to one call on one thread.

tests/resources.rs checks that the result is the same for `max_threads` 1 and 4, and that no
`thread::spawn`, `thread::scope` or `thread::Builder` appears in `src/`.
