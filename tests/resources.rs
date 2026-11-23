//! Resource controls: a counting global allocator measures the real peak of `read` and `strip`
//! and checks it against `max_alloc`; the result does not depend on `max_threads`; the crate
//! spawns no thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use exif::{Error, Keep, Limit, Limits};

// ---- counting allocator -------------------------------------------------------------------

static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

fn grow(n: usize) {
    let now = CUR.fetch_add(n, Ordering::Relaxed) + n;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

// SAFETY: every method forwards to `System` with the same arguments and only adds counters.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds the `GlobalAlloc::alloc` contract; passed through.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grow(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` come from a matching `alloc`, which was `System.alloc`.
        unsafe { System.dealloc(ptr, layout) };
        CUR.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller upholds the `GlobalAlloc::realloc` contract; passed through.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            if new_size >= layout.size() {
                grow(new_size - layout.size());
            } else {
                CUR.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// The tests of this file share one counter, so they run one at a time.
static LOCK: Mutex<()> = Mutex::new(());

/// Runs `f` and returns its result and the peak of live bytes above the level when it started.
/// The result is still alive when the peak is read, so it counts.
fn measured<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let base = CUR.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let r = f();
    (r, PEAK.load(Ordering::Relaxed).saturating_sub(base))
}

/// Allocations that are not charged to the budget and not sized from the input: the fixed fields
/// of `Metadata`, `Raw`, `Exif` and `Xmp`, small per-value headers, the `String` of a property,
/// the doubling of a small `Vec`. The test inputs have tens of entries and properties, so these
/// stay a few KiB; 32 KiB leaves room without hiding an input-sized (tens of KiB) uncharged copy.
const SLACK: usize = 32 << 10;

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

// ---- synthetic inputs ---------------------------------------------------------------------

fn entry(tag: u16, typ: u16, count: u32, val: u32) -> [u8; 12] {
    let mut e = [0u8; 12];
    e[0..2].copy_from_slice(&tag.to_le_bytes());
    e[2..4].copy_from_slice(&typ.to_le_bytes());
    e[4..8].copy_from_slice(&count.to_le_bytes());
    e[8..12].copy_from_slice(&val.to_le_bytes());
    e
}

/// A little-endian TIFF block: IFD0 with Orientation (6), `n` SHORT entries, and, when `pad` is
/// non-zero, one ASCII entry whose `pad` bytes are stored after the IFD.
fn tiff(n: usize, pad: usize) -> Vec<u8> {
    let count = n + 1 + usize::from(pad > 0);
    let data_off = 8 + 2 + 12 * count + 4;
    let mut v = b"II*\0".to_vec();
    v.extend_from_slice(&8u32.to_le_bytes());
    v.extend_from_slice(&(count as u16).to_le_bytes());
    v.extend_from_slice(&entry(0x0112, 3, 1, 6));
    for i in 0..n {
        v.extend_from_slice(&entry(0x1000 + i as u16, 3, 1, i as u32));
    }
    if pad > 0 {
        v.extend_from_slice(&entry(0x9999, 2, pad as u32, data_off as u32));
    }
    v.extend_from_slice(&0u32.to_le_bytes());
    if pad > 0 {
        v.resize(v.len() + pad - 1, b'a');
        v.push(0);
    }
    v
}

/// An XMP packet with Orientation and `n` simple properties.
fn xmp(n: usize) -> Vec<u8> {
    let mut s = String::from(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\
         <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
         <rdf:Description rdf:about=\"\" xmlns:tiff=\"http://ns.adobe.com/tiff/1.0/\" \
         xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" tiff:Orientation=\"6\">",
    );
    for i in 0..n {
        let _ = write!(s, "<xmp:P{i}>value {i}</xmp:P{i}>");
    }
    s.push_str("</rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>");
    s.into_bytes()
}

fn segment(marker: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![0xFF, marker];
    v.extend_from_slice(&(body.len() as u16 + 2).to_be_bytes());
    v.extend_from_slice(body);
    v
}

/// SOI, APP1 Exif, APP1 XMP, DQT, SOF0, SOS, `scan` bytes of scan data (no 0xFF), EOI.
fn jpeg(tiff: &[u8], xmp: &[u8], scan: usize) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8];
    v.extend(segment(0xE1, &[&b"Exif\0\0"[..], tiff].concat()));
    v.extend(segment(
        0xE1,
        &[&b"http://ns.adobe.com/xap/1.0/\0"[..], xmp].concat(),
    ));
    v.extend(segment(0xDB, &[&[0u8][..], &[16u8; 64]].concat()));
    v.extend(segment(0xC0, &[8, 0, 1, 0, 1, 1, 1, 0x11, 0]));
    v.extend(segment(0xDA, &[1, 1, 0, 0, 63, 0]));
    v.resize(v.len() + scan, 0x55);
    v.extend_from_slice(&[0xFF, 0xD9]);
    v
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in bytes {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = (data.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(data);
    let crc = crc32(&v[4..]);
    v.extend_from_slice(&crc.to_be_bytes());
    v
}

/// PNG signature, IHDR, eXIf, iTXt (uncompressed `XML:com.adobe.xmp`), IDAT, IEND.
fn png(tiff: &[u8], xmp: &[u8]) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let ihdr = [
        &1u32.to_be_bytes()[..],
        &1u32.to_be_bytes(),
        &[8, 0, 0, 0, 0],
    ]
    .concat();
    v.extend(chunk(b"IHDR", &ihdr));
    v.extend(chunk(b"eXIf", tiff));
    v.extend(chunk(
        b"iTXt",
        &[&b"XML:com.adobe.xmp\0\0\0\0\0"[..], xmp].concat(),
    ));
    v.extend(chunk(b"IDAT", &[0x55; 64]));
    v.extend(chunk(b"IEND", &[]));
    v
}

fn limits(max_alloc: usize, max_threads: usize) -> Limits {
    Limits {
        max_alloc,
        max_threads,
        ..Limits::default()
    }
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn within(what: &str, peak: usize, max_alloc: usize) {
    assert!(
        peak <= max_alloc.saturating_add(SLACK),
        "{what}: peak {peak} > max_alloc {max_alloc} + slack {SLACK}"
    );
}

// ---- tests --------------------------------------------------------------------------------

// Small inputs fit every budget: the call succeeds and its measured peak stays within max_alloc.
#[test]
fn small_inputs_peak_within_max_alloc() {
    let _g = lock();
    let inputs = [
        ("jpeg", jpeg(&tiff(50, 0), &xmp(20), 64)),
        ("png", png(&tiff(50, 0), &xmp(20))),
    ];
    for max_alloc in [64 << 10, 1 << 20, Limits::default().max_alloc] {
        let lim = limits(max_alloc, 1);
        for (name, data) in &inputs {
            let at = format!("{name} max_alloc {max_alloc}");
            let (r, peak) = measured(|| exif::read(data, &lim, &no_cancel()));
            let meta = r.unwrap_or_else(|e| panic!("{at}: read: {e}"));
            assert!(meta.exif.is_some() && meta.xmp.is_some(), "{at}: parsed");
            within(&format!("{at} read"), peak, max_alloc);
            drop(meta);

            let (r, peak) = measured(|| exif::strip(data, Keep::NONE, &lim, &no_cancel()));
            let out = r.unwrap_or_else(|e| panic!("{at}: strip: {e}"));
            assert!(out.len() < data.len(), "{at}: strip shrinks the file");
            within(&format!("{at} strip"), peak, max_alloc);

            let keep = Keep {
                orientation: true,
                icc: true,
                ..Keep::NONE
            };
            let (r, peak) = measured(|| exif::strip(data, keep, &lim, &no_cancel()));
            assert!(r.is_ok(), "{at}: strip keep: {r:?}");
            within(&format!("{at} strip keep"), peak, max_alloc);

            let (r, peak) = measured(|| exif::strip(data, Keep::default(), &lim, &no_cancel()));
            assert!(r.is_ok(), "{at}: strip default: {r:?}");
            within(&format!("{at} strip default"), peak, max_alloc);
        }
    }
}

// A max_alloc smaller than the data gives LimitExceeded(Alloc), and the peak still holds: the
// charge comes before the allocation.
#[test]
fn max_alloc_smaller_than_data_is_alloc_error() {
    let _g = lock();
    let alloc_err = Err(Error::LimitExceeded(Limit::Alloc));
    // PNG: a 100 KiB eXIf block and a 20 KB+ XMP packet; JPEG: 200 KiB of scan data, which a
    // rewrite must hold in its output.
    let png_big = png(&tiff(50, 100 << 10), &xmp(1000));
    let jpeg_big = jpeg(&tiff(50, 0), &xmp(20), 200 << 10);

    let lim = limits(64 << 10, 1);
    let (r, peak) = measured(|| exif::read(&png_big, &lim, &no_cancel()).map(|_| ()));
    assert_eq!(r, alloc_err, "read png at 64 KiB");
    within("read png at 64 KiB", peak, lim.max_alloc);

    let (r, peak) = measured(|| exif::strip(&jpeg_big, Keep::NONE, &lim, &no_cancel()).map(|_| ()));
    assert_eq!(r, alloc_err, "strip jpeg at 64 KiB");
    within("strip jpeg at 64 KiB", peak, lim.max_alloc);

    // The other pairs may succeed or hit the limit at 64 KiB; the peak holds either way.
    let (r, peak) = measured(|| exif::read(&jpeg_big, &lim, &no_cancel()).map(|_| ()));
    assert!(r.is_ok() || r == alloc_err, "read jpeg at 64 KiB: {r:?}");
    within("read jpeg at 64 KiB", peak, lim.max_alloc);
    let (r, peak) = measured(|| exif::strip(&png_big, Keep::NONE, &lim, &no_cancel()).map(|_| ()));
    assert!(r.is_ok() || r == alloc_err, "strip png at 64 KiB: {r:?}");
    within("strip png at 64 KiB", peak, lim.max_alloc);

    // 1 MiB and the default hold all of them.
    for max_alloc in [1 << 20, Limits::default().max_alloc] {
        let lim = limits(max_alloc, 1);
        for (name, data) in [("png", &png_big), ("jpeg", &jpeg_big)] {
            let at = format!("{name} max_alloc {max_alloc}");
            let (r, peak) = measured(|| exif::read(data, &lim, &no_cancel()));
            assert!(r.is_ok(), "{at}: read: {:?}", r.as_ref().map(|_| ()));
            within(&format!("{at} read"), peak, max_alloc);
            drop(r);
            let (r, peak) = measured(|| exif::strip(data, Keep::NONE, &lim, &no_cancel()));
            assert!(r.is_ok(), "{at}: strip: {:?}", r.as_ref().map(|_| ()));
            within(&format!("{at} strip"), peak, max_alloc);
        }
    }
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// A zlib stream of stored (uncompressed) deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut v = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = data.chunks(65535).collect();
    for (i, b) in blocks.iter().enumerate() {
        v.push(u8::from(i + 1 == blocks.len()));
        let n = b.len() as u16;
        v.extend_from_slice(&n.to_le_bytes());
        v.extend_from_slice(&(!n).to_le_bytes());
        v.extend_from_slice(b);
    }
    v.extend_from_slice(&adler32(data).to_be_bytes());
    v
}

// The inflate buffer grows by doubling, so an iCCP profile just past a doubling leaves up to as
// much spare capacity as it has bytes. That capacity is charged, so what the call reads after it
// (here a 120 000-byte eXIf block) cannot take the real peak past max_alloc.
#[test]
fn inflated_chunk_capacity_is_charged() {
    let _g = lock();
    let profile = vec![0u8; 2 * 65535 + 3];
    let mut iccp = b"icc\0\0".to_vec();
    iccp.extend(zlib_stored(&profile));
    let mut exif_block = tiff(50, 0);
    exif_block.resize(120_000, 0);
    let mut data = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let ihdr = [
        &1u32.to_be_bytes()[..],
        &1u32.to_be_bytes(),
        &[8, 0, 0, 0, 0],
    ]
    .concat();
    data.extend(chunk(b"IHDR", &ihdr));
    data.extend(chunk(b"iCCP", &iccp));
    data.extend(chunk(b"eXIf", &exif_block));
    data.extend(chunk(b"IDAT", &[0x55; 64]));
    data.extend(chunk(b"IEND", &[]));
    let lim = limits(256 << 10, 1);
    let (r, peak) = measured(|| exif::read(&data, &lim, &no_cancel()));
    let meta = r.unwrap_or_else(|e| panic!("read: {e}"));
    assert_eq!(meta.raw.icc.as_ref().map(Vec::len), Some(profile.len()));
    assert!(meta.exif.is_some());
    within("read png with iCCP", peak, lim.max_alloc);
}

// The XMP element stack keeps its capacity after the elements close; that capacity stays
// charged, so properties that follow cannot take the real peak past max_alloc. Each of the 300
// properties copies a 4 KiB namespace URI (1.2 MB in all, over the 1 MiB budget).
#[test]
fn xmp_stack_capacity_stays_charged() {
    let _g = lock();
    let mut s = String::from(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
         <rdf:Description xmlns:a=\"http://a/\">",
    );
    s.push_str(&"<a:p>".repeat(4096));
    s.push_str(&"</a:p>".repeat(4096));
    let _ = write!(
        s,
        "</rdf:Description><rdf:Description xmlns:b=\"http://b/{}\"",
        "u".repeat(4096)
    );
    for i in 0..300 {
        let _ = write!(s, " b:k{i}=\"\"");
    }
    s.push_str("/></rdf:RDF></x:xmpmeta>");
    let lim = limits(1 << 20, 1);
    let (r, peak) = measured(|| exif::Xmp::parse(s.as_bytes(), None, &lim, &no_cancel()));
    assert_eq!(r, Err(Error::LimitExceeded(Limit::Alloc)));
    within("xmp nested then many properties", peak, lim.max_alloc);
}

// reset_orientation charges its output and its XMP scan.
#[test]
fn reset_orientation_peak_within_max_alloc() {
    let _g = lock();
    let inputs = [
        ("jpeg", jpeg(&tiff(50, 0), &xmp(20), 64 << 10)),
        ("png", png(&tiff(50, 0), &xmp(1000))),
    ];
    for max_alloc in [64 << 10, 1 << 20] {
        let lim = limits(max_alloc, 1);
        for (name, data) in &inputs {
            let (r, peak) = measured(|| exif::reset_orientation(data, &lim, &no_cancel()));
            assert!(
                r.is_ok() || r == Err(Error::LimitExceeded(Limit::Alloc)),
                "{name} at {max_alloc}: {:?}",
                r.map(|_| ())
            );
            within(&format!("{name} reset at {max_alloc}"), peak, max_alloc);
        }
    }
}

// The crate spawns no thread, so max_threads 1 and 4 give the same result. This compares results;
// the no-spawn claim itself is `src_spawns_no_thread` below.
#[test]
fn same_result_for_max_threads_1_and_4() {
    let _g = lock();
    let inputs = [
        jpeg(&tiff(50, 0), &xmp(20), 64),
        png(&tiff(50, 0), &xmp(20)),
    ];
    for data in &inputs {
        let one = limits(64 << 20, 1);
        let four = limits(64 << 20, 4);
        let a = exif::read(data, &one, &no_cancel()).map(|m| format!("{m:?}"));
        let b = exif::read(data, &four, &no_cancel()).map(|m| format!("{m:?}"));
        assert!(a.is_ok(), "read: {a:?}");
        assert_eq!(a, b, "read differs between max_threads 1 and 4");
        let a = exif::strip(data, Keep::NONE, &one, &no_cancel());
        let b = exif::strip(data, Keep::NONE, &four, &no_cancel());
        assert!(a.is_ok(), "strip: {a:?}");
        assert_eq!(a, b, "strip differs between max_threads 1 and 4");
    }
}

// The grep-able claim of docs/threading.md: nothing in src/ starts a thread.
#[test]
fn src_spawns_no_thread() {
    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    rs_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(files.len() >= 5, "found only {} source files", files.len());
    let banned = ["thread::spawn", "thread::scope", "thread::Builder"];
    let mut hits = Vec::new();
    for f in &files {
        let Ok(text) = fs::read_to_string(f) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            let t = line.trim_start();
            if !t.starts_with("//") && banned.iter().any(|b| t.contains(b)) {
                hits.push(format!("{}:{}", f.display(), i + 1));
            }
        }
    }
    assert!(hits.is_empty(), "thread use in src: {hits:?}");
}
