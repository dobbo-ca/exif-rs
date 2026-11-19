//! The ExifTool parity oracle (docs/parity.md).
//!
//! `oracle read <in-dir> <out.jsonl>` writes one JSON object per file of `<in-dir>` (sorted by
//! name): `{"SourceFile":<name>, <key>:<value>, ...}` with the fields of the docs/parity.md table
//! (not every accessor: `Exif::get`/`entries` and `Xmp::orientation` are not called), keyed and
//! formatted as `exiftool -j -n -G1 -a -b` prints it, or `{"SourceFile":<name>,"Error":<class>}`.
//!
//! `oracle strip <all|keep|orient1> <in-dir> <out-dir>` writes `<out-dir>/<name>` for every file
//! that `strip` (`all`: `Keep::default()`, `keep`: orientation and ICC kept) or
//! `reset_orientation` (`orient1`) accepts, and one `<name>\t<class>` line per error to
//! `<out-dir>.errors`.
//!
//! The ExifTool value rules below are written new from a reading of ExifTool 13.64's behaviour
//! (docs/parity.md lists each one); no ExifTool code is copied. Output is deterministic and the
//! same on every platform (Rust float formatting does not depend on the C library).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::{env, fs, io, process};

use exif::{CaptureTime, Exif, Gps, Keep, Limits, Metadata, Rational, Xmp};

const NS_TIFF: &str = "http://ns.adobe.com/tiff/1.0/";
/// ExifTool's hdrgm tags that are lists (`List => 'Seq'`).
const HDRGM_LISTS: [&str; 5] = [
    "OffsetSDR",
    "OffsetHDR",
    "GainMapMin",
    "GainMapMax",
    "Gamma",
];
/// ExifTool's other hdrgm tags; `Version` has `Avoid`, so priority 0 (the first one wins).
const HDRGM_SCALARS: [&str; 4] = [
    "Version",
    "BaseRenditionIsHDR",
    "HDRCapacityMin",
    "HDRCapacityMax",
];

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["read", dir, out] => read_cmd(Path::new(dir), Path::new(out)),
        ["strip", mode @ ("all" | "keep" | "orient1"), dir, out] => {
            strip_cmd(mode, Path::new(dir), out)
        }
        _ => {
            eprintln!(
                "usage: oracle read <in-dir> <out.jsonl>\n       oracle strip <all|keep|orient1> <in-dir> <out-dir>"
            );
            process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("oracle: {e}");
        process::exit(1);
    }
}

fn limits() -> Limits {
    Limits {
        max_threads: 1,
        ..Limits::default()
    }
}

/// The regular files of `dir` (symlinks followed), sorted by name.
fn files(dir: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if fs::metadata(&path).is_ok_and(|m| m.is_file()) {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            out.push((name, path));
        }
    }
    out.sort();
    Ok(out)
}

fn read_cmd(dir: &Path, out: &Path) -> io::Result<()> {
    let limits = limits();
    let cancel = AtomicBool::new(false);
    let mut text = String::new();
    for (name, path) in files(dir)? {
        let data = fs::read(&path)?;
        let meta = exif::read(&data, &limits, &cancel);
        let fields = match &meta {
            Ok(meta) => fields(meta),
            Err(e) => vec![("Error".to_string(), Val::Text(e.class().into()))],
        };
        text.push_str("{\"SourceFile\":");
        json_string(&mut text, name.as_bytes());
        for (key, val) in &fields {
            text.push_str(",\"");
            text.push_str(key);
            text.push_str("\":");
            val.write(&mut text);
        }
        text.push_str("}\n");
    }
    fs::write(out, text)
}

fn strip_cmd(mode: &str, dir: &Path, out: &str) -> io::Result<()> {
    let out = out.trim_end_matches('/');
    fs::create_dir_all(out)?;
    let limits = limits();
    let cancel = AtomicBool::new(false);
    let mut errors = String::new();
    for (name, path) in files(dir)? {
        let data = fs::read(&path)?;
        let result = match mode {
            "all" => exif::strip(&data, Keep::default(), &limits, &cancel),
            "keep" => exif::strip(
                &data,
                Keep {
                    orientation: true,
                    icc: true,
                },
                &limits,
                &cancel,
            ),
            _ => exif::reset_orientation(&data, &limits, &cancel),
        };
        match result {
            Ok(bytes) => fs::write(Path::new(out).join(&name), bytes)?,
            Err(e) => {
                let _ = writeln!(errors, "{name}\t{}", e.class());
            }
        }
    }
    fs::write(format!("{out}.errors"), errors)
}

/// One JSON value: ExifTool text (number, boolean or string by ExifTool's rule), a binary
/// value (base64 unless it is printable UTF-8), or a list.
enum Val<'a> {
    Text(Vec<u8>),
    Binary(&'a [u8]),
    List(Vec<Vec<u8>>),
}

impl Val<'_> {
    fn write(&self, out: &mut String) {
        match self {
            Val::Text(v) => json_value(out, v),
            Val::Binary(v) => json_value(out, &convert_binary(v)),
            Val::List(vs) => {
                out.push('[');
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    json_value(out, v);
                }
                out.push(']');
            }
        }
    }
}

fn put(f: &mut Vec<(String, Val<'_>)>, key: &str, v: Option<Vec<u8>>) {
    if let Some(v) = v {
        f.push((key.to_string(), Val::Text(v)));
    }
}

fn fields(meta: &Metadata) -> Vec<(String, Val<'_>)> {
    let mut f = Vec::new();
    if let Some(ex) = &meta.exif {
        exif_fields(ex, &mut f);
    }
    if let Some(xmp) = &meta.xmp {
        xmp_fields(xmp, &mut f);
    }
    // The blocks ExifTool prints only when asked for by name (`-EXIF -XMP -ICC_Profile`);
    // ExifTool makes the EXIF tag only for a block of more than 16 bytes.
    let raw = &meta.raw;
    if let Some(b) = raw.exif.as_deref().filter(|b| b.len() > 16) {
        let v = kodak_patch(meta.exif.as_ref(), b)
            .map_or(Val::Binary(b), |p| Val::Text(convert_binary(&p)));
        f.push(("EXIF:EXIF".into(), v));
    }
    if let Some(b) = raw.xmp.as_deref() {
        f.push(("XMP:XMP".into(), Val::Binary(b)));
    }
    if let Some(b) = raw.icc.as_deref() {
        f.push(("ICC_Profile:ICC_Profile".into(), Val::Binary(b)));
    }
    f
}

/// The EXIF block as ExifTool prints it after its maker note handling changed it in place:
/// MakerNoteKodak8b (Make has `Kodak`, note `MM\0*\0\0\0\x08\0?\0\0`) and MakerNoteKodak11 (Make
/// without `Kodak`, which MakerNoteKodak8c would take, Model has `Kodak` or `PixPro`, note
/// `II*\0\x08\0\0\0?\0\0\0`) go through ExifTool's `ProcessKodakPatch`, which writes the 2-byte
/// entry count at note offset 10 (a copy of bytes 8..10 for these headers). `None`: unchanged.
fn kodak_patch(ex: Option<&Exif>, block: &[u8]) -> Option<Vec<u8>> {
    let ex = ex?;
    let note = ex.maker_note()?;
    let kodak = ex.make()?.to_ascii_lowercase().contains("kodak");
    let model = ex.model().unwrap_or_default().to_ascii_lowercase();
    let k8b =
        kodak && note.starts_with(b"MM\0*\0\0\0\x08\0") && note.get(10..12) == Some(&[0u8, 0][..]);
    let k11 = !kodak
        && (model.contains("kodak") || model.contains("pixpro"))
        && note.starts_with(b"II*\0\x08\0\0\0")
        && note.get(9..12) == Some(&[0u8, 0, 0][..]);
    if !(k8b || k11) {
        return None;
    }
    let at = block.windows(note.len()).position(|w| w == note)?;
    let mut out = block.to_vec();
    out.copy_within(at + 8..at + 10, at + 10);
    Some(out)
}

/// A rational as ExifTool reads it, as bytes.
fn rat(r: Rational) -> Vec<u8> {
    rational(i64::from(r.num), i64::from(r.den)).into_bytes()
}

/// Rationals as ExifTool reads several values of one entry: joined with a space.
fn rats(c: &[Rational]) -> Vec<u8> {
    c.iter()
        .map(|r| rational(i64::from(r.num), i64::from(r.den)))
        .collect::<Vec<_>>()
        .join(" ")
        .into_bytes()
}

fn num(n: impl ToString) -> Vec<u8> {
    n.to_string().into_bytes()
}

fn text(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

/// Every value comes from the public accessor that claims its key; the oracle only formats it as
/// ExifTool prints it. An accessor that gives `None` gives no key, so a value ExifTool reads and
/// the accessor does not is a mismatch.
fn exif_fields<'a>(ex: &'a Exif, f: &mut Vec<(String, Val<'a>)>) {
    put(f, "IFD0:Orientation", ex.orientation().map(num));
    put(f, "IFD0:Make", ex.make().map(text));
    put(f, "IFD0:Model", ex.model().map(text));

    if let Some(ct) = ex.capture_time() {
        put(f, "ExifIFD:DateTimeOriginal", Some(text(&ct.date_time)));
        put(
            f,
            "ExifIFD:SubSecTimeOriginal",
            ct.subsec.as_deref().map(text),
        );
        put(
            f,
            "ExifIFD:OffsetTimeOriginal",
            ct.offset.as_deref().map(text),
        );
        put(
            f,
            "Composite:SubSecDateTimeOriginal",
            sub_sec_date_time(&ct),
        );
    }
    let x = ex.exposure();
    put(f, "ExifIFD:ExposureTime", x.exposure_time.map(rat));
    put(f, "ExifIFD:FNumber", x.f_number.map(rat));
    put(f, "ExifIFD:ISO", x.iso.map(num));
    put(f, "ExifIFD:FocalLength", x.focal_length.map(rat));
    if let Some((w, h)) = ex.pixel_size() {
        put(f, "ExifIFD:ExifImageWidth", Some(num(w)));
        put(f, "ExifIFD:ExifImageHeight", Some(num(h)));
    }
    put(f, "ExifIFD:LensMake", ex.lens_make().map(text));
    put(f, "ExifIFD:LensModel", ex.lens_model().map(text));

    if let Some(gps) = ex.gps() {
        gps_fields(&gps, f);
    }
    if let Some(t) = ex.thumbnail().filter(|t| !t.is_empty()) {
        f.push(("IFD1:ThumbnailImage".into(), Val::Binary(t)));
    }
}

fn gps_fields(gps: &Gps, f: &mut Vec<(String, Val<'_>)>) {
    let lat = gps.latitude.map(|c| to_degrees(&rats(&c)));
    let lon = gps.longitude.map(|c| to_degrees(&rats(&c)));
    let time = gps.time_stamp.and_then(|c| convert_time_stamp(&rats(&c)));
    let date = gps.date_stamp.as_deref().map(|d| exif_date(d.as_bytes()));

    // Composite GPSLatitude/GPSLongitude need the coordinate and its ref; GPSAltitude the
    // altitude and its ref.
    if lat.is_some() && gps.latitude_ref.is_some() {
        put(
            f,
            "Composite:GPSLatitude",
            Some(composite_deg(gps.latitude_deg(), gps.latitude_ref, b'S')),
        );
    }
    if lon.is_some() && gps.longitude_ref.is_some() {
        put(
            f,
            "Composite:GPSLongitude",
            Some(composite_deg(gps.longitude_deg(), gps.longitude_ref, b'W')),
        );
    }
    if gps.altitude.is_some() && gps.altitude_ref.is_some() {
        // ExifTool returns GPSAltitude's own %.10g text when the ref is 0, else the number
        // -abs(value), which Perl prints with %.15g.
        let v = gps.altitude_m().filter(|a| a.is_finite()).map(|a| {
            if a.is_sign_negative() {
                perl_nv(a).into_bytes()
            } else {
                fmt_g(a, 10).into_bytes()
            }
        });
        put(f, "Composite:GPSAltitude", v);
    }
    if let (Some(d), Some(t)) = (&date, &time) {
        let mut v = d.clone();
        v.push(b' ');
        v.extend_from_slice(t);
        v.push(b'Z');
        put(f, "Composite:GPSDateTime", Some(v));
    }
    put(f, "GPS:GPSLatitudeRef", gps.latitude_ref.map(|r| vec![r]));
    put(f, "GPS:GPSLatitude", lat);
    put(f, "GPS:GPSLongitudeRef", gps.longitude_ref.map(|r| vec![r]));
    put(f, "GPS:GPSLongitude", lon);
    put(f, "GPS:GPSAltitudeRef", gps.altitude_ref.map(num));
    put(f, "GPS:GPSAltitude", gps.altitude.map(rat));
    put(f, "GPS:GPSTimeStamp", time);
    put(f, "GPS:GPSDateStamp", date);
}

/// A Composite GPS coordinate (ValueConv `$val[1] =~ /^S/i ? -$val[0] : $val[0]`, `W` for the
/// longitude), given the coordinate is present and the ref too: the degrees as a Perl number.
/// Degrees that are not computable (a rational `x/0`) are ToDegrees' `''` on the GPS key, so the
/// Composite is `''`, or for an `S`/`W` ref Perl's `-''`, the number -0, printed `0`.
fn composite_deg(d: Option<f64>, r: Option<u8>, neg: u8) -> Vec<u8> {
    match d {
        Some(d) => perl_nv(d).into_bytes(),
        None if r.is_some_and(|r| r.eq_ignore_ascii_case(&neg)) => b"0".to_vec(),
        None => Vec::new(),
    }
}

fn xmp_fields<'a>(xmp: &'a Xmp, f: &mut Vec<(String, Val<'a>)>) {
    // Xmp::get, create_date and rating follow ExifTool's priorities: tiff:Orientation and
    // xmp:CreateDate the first, xmp:Rating the last.
    put(
        f,
        "XMP-tiff:Orientation",
        xmp.get(NS_TIFF, "Orientation").map(text),
    );
    put(
        f,
        "XMP-xmp:CreateDate",
        xmp.create_date()
            .map(|v| convert_xmp_date(v, false).into_bytes()),
    );
    put(f, "XMP-xmp:Rating", xmp.rating().map(text));

    let mut names: Vec<&str> = Vec::new();
    for p in xmp.hdrgm() {
        if !names.contains(&p.name.as_str()) {
            names.push(&p.name);
        }
    }
    for name in names {
        let values: Vec<&str> = xmp
            .hdrgm()
            .filter(|p| p.name == name)
            .map(|p| p.value.as_str())
            .collect();
        let val = if HDRGM_LISTS.contains(&name) {
            list_or_one(values.iter().map(|v| v.as_bytes().to_vec()).collect())
        } else if HDRGM_SCALARS.contains(&name) {
            let v = if name == "Version" {
                values.first()
            } else {
                values.last()
            };
            Val::Text(v.map(|v| v.as_bytes().to_vec()).unwrap_or_default())
        } else {
            // An unknown property: ExifTool's XMPAutoConv, and a list when it repeats.
            list_or_one(values.iter().map(|v| auto_conv(v).into_bytes()).collect())
        };
        f.push((format!("XMP-hdrgm:{}", ucfirst(name)), val));
    }
}

/// ExifTool prints a list of one item as the item.
fn list_or_one(mut vs: Vec<Vec<u8>>) -> Val<'static> {
    if vs.len() == 1 {
        Val::Text(vs.remove(0))
    } else {
        Val::List(vs)
    }
}

fn ucfirst(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + c.as_str(),
        None => String::new(),
    }
}

/// A rational as ExifTool reads it. An exact integer quotient is a Perl integer, so 0/x is
/// `0`, never `-0`.
fn rational(num: i64, den: i64) -> String {
    match (num, den) {
        (0, 0) => "undef".into(),
        (_, 0) => "inf".into(),
        (0, _) => "0".into(),
        _ => fmt_g(num as f64 / den as f64, 10),
    }
}

/// C's `%.<p>g` for a finite number.
fn fmt_g(x: f64, p: usize) -> String {
    let e = format!("{:.*e}", p - 1, x);
    let (mant, exp) = e.split_once('e').unwrap_or((e.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if exp < -4 || exp >= p as i32 {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim_frac(mant), exp.unsigned_abs())
    } else {
        let prec = usize::try_from(p as i32 - 1 - exp).unwrap_or(0);
        trim_frac(&format!("{x:.prec$}")).to_string()
    }
}

fn trim_frac(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

/// How Perl prints a floating-point number: `%.15g`, `0` for both zeros, `Inf`, `-Inf`, `NaN`.
fn perl_nv(x: f64) -> String {
    if x == 0.0 {
        "0".into()
    } else if x.is_nan() {
        "NaN".into()
    } else if x == f64::INFINITY {
        "Inf".into()
    } else if x == f64::NEG_INFINITY {
        "-Inf".into()
    } else {
        fmt_g(x, 15)
    }
}

/// Perl's `\s` on a byte string.
fn is_perl_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Perl numification of a string: the longest number at the start, `Inf`/`NaN`, else 0.
fn perl_num(t: &[u8]) -> f64 {
    let s = String::from_utf8_lossy(t);
    let s: &str = &s;
    let neg = s.starts_with('-');
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    let lower = body.to_ascii_lowercase();
    if lower.starts_with("inf") {
        return if neg {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    if lower.starts_with("nan") {
        return f64::NAN;
    }
    let b = body.as_bytes();
    let digits = |i: usize| b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    let int = digits(0);
    let mut i = int;
    if b.get(i) == Some(&b'.') {
        i += 1 + digits(i + 1);
    }
    if i == 0 || (int == 0 && i == 1) {
        return 0.0;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let j = i + 1 + usize::from(matches!(b.get(i + 1), Some(b'+' | b'-')));
        let k = digits(j.min(b.len()));
        if k > 0 {
            i = j + k;
        }
    }
    let v: f64 = body[..i].parse().unwrap_or(0.0);
    if neg { -v } else { v }
}

/// The numbers ExifTool's `ToDegrees` finds: `[+-]?(?=\d|\.\d)\d*(\.\d*)?([Ee][+-]\d+)?`.
fn numbers(s: &[u8]) -> Vec<f64> {
    let digit = |i: usize| s.get(i).is_some_and(u8::is_ascii_digit);
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let mut j = i + usize::from(matches!(s[i], b'+' | b'-'));
        if !(digit(j) || (s.get(j) == Some(&b'.') && digit(j + 1))) {
            i += 1;
            continue;
        }
        while digit(j) {
            j += 1;
        }
        if s.get(j) == Some(&b'.') {
            j += 1;
            while digit(j) {
                j += 1;
            }
        }
        if matches!(s.get(j), Some(b'e' | b'E'))
            && matches!(s.get(j + 1), Some(b'+' | b'-'))
            && digit(j + 2)
        {
            j += 2;
            while digit(j) {
                j += 1;
            }
        }
        out.push(
            std::str::from_utf8(&s[i..j])
                .ok()
                .and_then(|t| t.parse().ok())
                .unwrap_or(0.0),
        );
        i = j;
    }
    out
}

/// `\b<word>\b` in `s`.
fn has_word(s: &[u8], word: &[u8]) -> bool {
    let is_word = |b: Option<&u8>| b.is_some_and(|&b| b.is_ascii_alphanumeric() || b == b'_');
    (0..s.len()).any(|i| {
        s[i..].starts_with(word)
            && !is_word(i.checked_sub(1).and_then(|p| s.get(p)))
            && !is_word(s.get(i + word.len()))
    })
}

/// ExifTool's GPS `ToDegrees` (ValueConv of GPSLatitude and GPSLongitude): `''` when the value
/// holds `inf` or `undef`, else `d + (m + s/60)/60` of the first three numbers.
fn to_degrees(v: &[u8]) -> Vec<u8> {
    if has_word(v, b"inf") || has_word(v, b"undef") {
        return Vec::new();
    }
    let n = numbers(v);
    let Some(&d) = n.first() else {
        return Vec::new();
    };
    let g = |i: usize| n.get(i).copied().unwrap_or(0.0);
    perl_nv(d + (g(1) + g(2) / 60.0) / 60.0).into_bytes()
}

/// `%.2d`.
fn d2(x: f64) -> String {
    let n = x as i64;
    if n < 0 {
        format!("-{:02}", n.unsigned_abs())
    } else {
        format!("{n:02}")
    }
}

/// ExifTool's `GPS::ConvertTimeStamp` (ValueConv of GPSTimeStamp): `HH:MM:SS[.fff]`, seconds
/// with `%012.9f` and trailing zeros removed. `None` when a part is not a finite number (Perl
/// would print Inf/NaN arithmetic, which is not reproduced).
fn convert_time_stamp(v: &[u8]) -> Option<Vec<u8>> {
    let t: Vec<f64> = v
        .split(|&b| is_perl_space(b))
        .filter(|t| !t.is_empty())
        .map(perl_num)
        .collect();
    let g = |i: usize| t.get(i).copied().unwrap_or(0.0);
    let mut f = (g(0) * 60.0 + g(1)) * 60.0 + g(2);
    if !f.is_finite() {
        return None;
    }
    let mut h = (f / 3600.0).trunc();
    f -= h * 3600.0;
    let mut m = (f / 60.0).trunc();
    f -= m * 60.0;
    let mut ss = format!("{f:012.9}");
    if perl_num(ss.as_bytes()) >= 60.0 {
        ss = "00".into();
        m += 1.0;
        if m >= 60.0 {
            m -= 60.0;
            h += 1.0;
        }
    } else {
        let t = ss.trim_end_matches('0');
        if t.len() < ss.len() {
            ss = t.strip_suffix('.').unwrap_or(t).to_string();
        }
    }
    Some(format!("{}:{}:{ss}", d2(h), d2(m)).into_bytes())
}

/// ExifTool's `Exif::ExifDate`: `s/(\d{4})[^\d]*(\d{2})[^\d]*(\d{2})$/$1:$2:$3/`.
fn exif_date(s: &[u8]) -> Vec<u8> {
    let d = |i: usize| s.get(i).is_some_and(u8::is_ascii_digit);
    let skip = |mut i: usize| {
        while s.get(i).is_some_and(|b| !b.is_ascii_digit()) {
            i += 1;
        }
        i
    };
    for start in 0..s.len() {
        if !(0..4).all(|k| d(start + k)) {
            continue;
        }
        let m = skip(start + 4);
        if !(d(m) && d(m + 1)) {
            continue;
        }
        let day = skip(m + 2);
        if !(d(day) && d(day + 1)) {
            continue;
        }
        let end = day + 2;
        if end == s.len() || (end + 1 == s.len() && s[end] == b'\n') {
            let mut out = s[..start + 4].to_vec();
            out.push(b':');
            out.extend_from_slice(&s[m..m + 2]);
            out.push(b':');
            out.extend_from_slice(&s[day..]);
            return out;
        }
    }
    s.to_vec()
}

/// ExifTool's `XMP::ConvertXMPDate`: `YYYY-MM-DDThh:mm[:ss][tz]` becomes `YYYY:MM:DD hh:mm[:ss][tz]`;
/// otherwise (unless `unsure`) a value starting with four digits has every `-` changed to `:`.
fn convert_xmp_date(v: &str, unsure: bool) -> String {
    let b = v.as_bytes();
    let d = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    let at = |i: usize, c: u8| b.get(i) == Some(&c);
    if (0..4).all(d)
        && at(4, b'-')
        && d(5)
        && d(6)
        && at(7, b'-')
        && d(8)
        && d(9)
        && (at(10, b'T') || at(10, b' '))
        && d(11)
        && d(12)
        && at(13, b':')
        && d(14)
        && d(15)
    {
        let (secs, i) = if at(16, b':') && d(17) && d(18) {
            (&v[16..19], 19)
        } else {
            ("", 16)
        };
        let start = i + b[i..].iter().take_while(|&&c| is_perl_space(c)).count();
        let after = &v[start..];
        let tz = after.strip_suffix('\n').unwrap_or(after);
        if !tz.bytes().any(is_perl_space) {
            return format!(
                "{}:{}:{} {}{secs}{tz}",
                &v[0..4],
                &v[5..7],
                &v[8..10],
                &v[11..16]
            );
        }
    }
    if !unsure && (0..4).all(d) {
        return v.replace('-', ":");
    }
    v.to_string()
}

/// ExifTool's `XMP::ConvertRational`: `n/d` becomes the quotient (a Perl integer when exact),
/// `inf` (x/0) or `undef` (0/0).
fn convert_rational(v: &str) -> Option<String> {
    let s = v.strip_suffix('\n').unwrap_or(v);
    let (n, d) = s.split_once('/')?;
    let int = |t: &str| {
        let t = t.strip_prefix('-').unwrap_or(t);
        !t.is_empty() && t.bytes().all(|c| c.is_ascii_digit())
    };
    if !int(n) || !int(d) {
        return None;
    }
    let (nf, df): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    if df == 0.0 {
        return Some(if n == "0" { "undef" } else { "inf" }.into());
    }
    if let (Ok(ni), Ok(di)) = (n.parse::<i64>(), d.parse::<i64>())
        && ni.checked_rem(di) == Some(0)
    {
        return ni.checked_div(di).map(|q| q.to_string());
    }
    Some(perl_nv(nf / df))
}

/// ExifTool's XMPAutoConv for a property it has no definition for.
fn auto_conv(v: &str) -> String {
    convert_rational(v).unwrap_or_else(|| convert_xmp_date(v, true))
}

/// Composite:SubSecDateTimeOriginal (ExifTool's `%subSecConv` RawConv on the raw values): the
/// sub-seconds go after the first ` hh:mm:ss` not already followed by `.digits`, the offset
/// (`[-+]h[h]:mm`, as `%s%.2d:%.2d`) at the end when the date has no sign. `None` when neither
/// applies.
fn sub_sec_date_time(ct: &CaptureTime) -> Option<Vec<u8>> {
    let dto = ct.date_time.as_bytes();
    let mut v = None;
    if let Some(sub) = ct.subsec.as_deref() {
        let n = sub.bytes().take_while(u8::is_ascii_digit).count();
        if n > 0 {
            v = insert_subsec(dto, &sub.as_bytes()[..n]);
        }
    }
    if let Some((sign, h, m)) = ct.offset.as_deref().and_then(parse_offset)
        && !dto.contains(&b'-')
        && !dto.contains(&b'+')
    {
        let mut out = v.unwrap_or_else(|| dto.to_vec());
        out.extend_from_slice(format!("{sign}{h:02}:{m:02}").as_bytes());
        v = Some(out);
    }
    v
}

fn insert_subsec(s: &[u8], sub: &[u8]) -> Option<Vec<u8>> {
    let d = |i: usize| s.get(i).is_some_and(u8::is_ascii_digit);
    let at = |i: usize, c: u8| s.get(i) == Some(&c);
    let i = (0..s.len()).find(|&i| {
        at(i, b' ')
            && d(i + 1)
            && d(i + 2)
            && at(i + 3, b':')
            && d(i + 4)
            && d(i + 5)
            && at(i + 6, b':')
            && d(i + 7)
            && d(i + 8)
            && !(at(i + 9, b'.') && d(i + 10))
    })?;
    let mut out = s[..i + 9].to_vec();
    out.push(b'.');
    out.extend_from_slice(sub);
    out.extend_from_slice(&s[i + 9..]);
    Some(out)
}

/// `^([-+])(\d{1,2}):(\d{2})`.
fn parse_offset(s: &str) -> Option<(char, u32, u32)> {
    let sign = s.chars().next().filter(|c| matches!(c, '+' | '-'))?;
    let rest = &s[1..];
    let n = rest.bytes().take_while(u8::is_ascii_digit).count().min(2);
    let after = rest.get(n..)?;
    let m = after.strip_prefix(':')?.get(..2)?;
    if n == 0 || !m.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((sign, rest[..n].parse().ok()?, m.parse().ok()?))
}

/// ExifTool's JSON writer (`EscapeJSON` without quoting): `true`/`false` in any case as a JSON
/// boolean, a number by its regex unquoted, anything else a string.
fn json_value(out: &mut String, v: &[u8]) {
    if let Ok(s) = std::str::from_utf8(v) {
        // Perl's `$` also matches before a final newline; the newline is JSON white space.
        let t = s.strip_suffix('\n').unwrap_or(s);
        if t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("false") {
            out.push_str(&t.to_ascii_lowercase());
            return;
        }
        if is_json_number(t.as_bytes()) {
            out.push_str(t);
            return;
        }
    }
    json_string(out, v);
}

/// `^-?(\d|[1-9]\d{1,14})(\.\d{1,16})?(e[-+]?\d{1,3})?$`, `e` in either case.
fn is_json_number(b: &[u8]) -> bool {
    let digits = |i: usize| {
        b.get(i..)
            .map_or(0, |r| r.iter().take_while(|c| c.is_ascii_digit()).count())
    };
    let mut i = usize::from(b.first() == Some(&b'-'));
    let int = digits(i);
    if int == 0 || int > 15 || (int > 1 && b.get(i) == Some(&b'0')) {
        return false;
    }
    i += int;
    if b.get(i) == Some(&b'.') {
        let frac = digits(i + 1);
        if frac == 0 || frac > 16 {
            return false;
        }
        i += 1 + frac;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let exp = digits(i);
        if exp == 0 || exp > 3 {
            return false;
        }
        i += exp;
    }
    i == b.len()
}

/// A JSON string: base64 (`"base64:..."`) when the bytes are not UTF-8 by ExifTool's `IsUTF8`
/// (which also refuses U+FFFE and U+FFFF); else `"`, `\`, tab, LF and CR escaped, NULs removed,
/// other control characters as `\uXXXX`.
fn json_string(out: &mut String, v: &[u8]) {
    out.push('"');
    match std::str::from_utf8(v) {
        Ok(s) if !s.contains(['\u{FFFE}', '\u{FFFF}']) => {
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\t' => out.push_str("\\t"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\0' => {}
                    '\u{1}'..='\u{1f}' | '\u{7f}' => {
                        let _ = write!(out, "\\u{:04X}", u32::from(c));
                    }
                    c => out.push(c),
                }
            }
        }
        _ => {
            out.push_str("base64:");
            base64(out, v);
        }
    }
    out.push('"');
}

/// ExifTool's `ConvertBinary` for JSON: base64 unless every byte is tab, LF, CR, printable
/// ASCII or 0x80-0xF7 and the bytes are UTF-8.
fn convert_binary(v: &[u8]) -> Vec<u8> {
    let printable = v
        .iter()
        .all(|&b| matches!(b, 0x09 | 0x0a | 0x0d | 0x20..=0x7e | 0x80..=0xf7));
    match std::str::from_utf8(v) {
        Ok(s) if printable && !s.contains(['\u{FFFE}', '\u{FFFF}']) => v.to_vec(),
        _ => {
            let mut s = String::from("base64:");
            base64(&mut s, v);
            s.into_bytes()
        }
    }
}

/// Standard base64 with `=` padding, no line breaks.
fn base64(out: &mut String, v: &[u8]) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in v.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
}
