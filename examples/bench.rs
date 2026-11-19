//! `bench <dir>`: reads every file of `<dir>` (sorted) into memory, then times `exif::read` and
//! `exif::strip(Keep::default())` over all of them, one thread, three rounds, best of the three.
//! Prints a markdown table: files, MB, read ms, strip ms. tests/parity/bench.sh runs it.

use std::hint::black_box;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use exif::{Keep, Limits};

fn main() -> ExitCode {
    let Some(dir) = std::env::args_os().nth(1) else {
        eprintln!("usage: bench <dir>");
        return ExitCode::from(2);
    };
    let mut paths: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect(),
        Err(e) => {
            eprintln!("bench: {e}");
            return ExitCode::from(2);
        }
    };
    paths.sort();
    let files: Vec<Vec<u8>> = paths.iter().filter_map(|p| std::fs::read(p).ok()).collect();
    let bytes: usize = files.iter().map(Vec::len).sum();

    let limits = Limits {
        max_threads: 1,
        ..Limits::default()
    };
    let cancel = AtomicBool::new(false);
    let (mut read_ms, mut strip_ms) = (f64::MAX, f64::MAX);
    let (mut read_err, mut strip_err) = (0usize, 0usize);
    for _ in 0..3 {
        let t = Instant::now();
        read_err = files
            .iter()
            .filter(|f| black_box(exif::read(f, &limits, &cancel)).is_err())
            .count();
        read_ms = read_ms.min(t.elapsed().as_secs_f64() * 1e3);

        let t = Instant::now();
        strip_err = files
            .iter()
            .filter(|f| black_box(exif::strip(f, Keep::default(), &limits, &cancel)).is_err())
            .count();
        strip_ms = strip_ms.min(t.elapsed().as_secs_f64() * 1e3);
    }
    eprintln!("bench: {read_err} files failed read, {strip_err} failed strip");

    println!("| files | MB | read ms | strip ms |");
    println!("|---|---|---|---|");
    println!(
        "| {} | {:.1} | {read_ms:.1} | {strip_ms:.1} |",
        files.len(),
        bytes as f64 / 1e6
    );
    ExitCode::SUCCESS
}
