//! The parsed EXIF block: entries per IFD and typed accessors.
//!
//! String accessors follow ExifTool 13.64 (`-n` values): the ASCII value up to the first NUL;
//! `Make` and `Model` also lose trailing whitespace (ExifTool's `s/\s+$//`),
//! `SubSecTimeOriginal` loses trailing spaces, the other strings are returned as stored. A
//! string that is not valid UTF-8 gives `None`.

use std::sync::atomic::AtomicBool;

use crate::budget::Budget;
use crate::{Error, Limits};

/// The byte order of the TIFF block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ByteOrder {
    /// `II`, least significant byte first.
    Little,
    /// `MM`, most significant byte first.
    Big,
}

/// Which IFD an entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IfdKind {
    /// The first IFD (the main image).
    Ifd0,
    /// The Exif sub-IFD (IFD0 tag 0x8769).
    Exif,
    /// The GPS sub-IFD (IFD0 tag 0x8825).
    Gps,
    /// The Interoperability sub-IFD (Exif IFD tag 0xA005).
    Interop,
    /// The second IFD (the thumbnail), linked from IFD0.
    Ifd1,
}

impl IfdKind {
    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

/// An unsigned rational number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    /// Numerator.
    pub num: u32,
    /// Denominator.
    pub den: u32,
}

impl Rational {
    /// `num / den`, or `None` when the denominator is 0.
    pub fn to_f64(self) -> Option<f64> {
        if self.den == 0 {
            None
        } else {
            Some(f64::from(self.num) / f64::from(self.den))
        }
    }
}

/// A signed rational number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SRational {
    /// Numerator.
    pub num: i32,
    /// Denominator.
    pub den: i32,
}

/// The value of one entry, by TIFF type. Type 13 (IFD) is read as `Long`; entries of an unknown
/// type are not kept.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Type 1.
    Byte(Vec<u8>),
    /// Type 2, as stored (including any NUL).
    Ascii(Vec<u8>),
    /// Type 3.
    Short(Vec<u16>),
    /// Type 4 (and 13).
    Long(Vec<u32>),
    /// Type 5.
    Rational(Vec<Rational>),
    /// Type 6.
    SByte(Vec<i8>),
    /// Type 7.
    Undefined(Vec<u8>),
    /// Type 8.
    SShort(Vec<i16>),
    /// Type 9.
    SLong(Vec<i32>),
    /// Type 10.
    SRational(Vec<SRational>),
    /// Type 11.
    Float(Vec<f32>),
    /// Type 12.
    Double(Vec<f64>),
}

/// One IFD entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The tag number.
    pub tag: u16,
    /// The value.
    pub value: Value,
}

/// A parsed EXIF block.
#[derive(Debug, Clone, PartialEq)]
pub struct Exif {
    pub(crate) order: ByteOrder,
    pub(crate) ifds: [Vec<Entry>; 5],
    pub(crate) thumbnail: Option<Vec<u8>>,
}

/// When the picture was taken.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CaptureTime {
    /// Exif 0x9003 DateTimeOriginal, ASCII up to the first NUL, as stored.
    pub date_time: String,
    /// Exif 0x9291 SubSecTimeOriginal, trailing spaces removed (as ExifTool does).
    pub subsec: Option<String>,
    /// Exif 0x9011 OffsetTimeOriginal, as stored.
    pub offset: Option<String>,
}

/// Exposure values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Exposure {
    /// Exif 0x829A.
    pub exposure_time: Option<Rational>,
    /// Exif 0x829D.
    pub f_number: Option<Rational>,
    /// Exif 0x8827 (first value, SHORT or LONG).
    pub iso: Option<u32>,
    /// Exif 0x920A.
    pub focal_length: Option<Rational>,
}

/// GPS values of the GPS IFD.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Gps {
    /// 0x0002: degrees, minutes, seconds.
    pub latitude: Option<[Rational; 3]>,
    /// 0x0001: `b'N'` or `b'S'`.
    pub latitude_ref: Option<u8>,
    /// 0x0004: degrees, minutes, seconds.
    pub longitude: Option<[Rational; 3]>,
    /// 0x0003: `b'E'` or `b'W'`.
    pub longitude_ref: Option<u8>,
    /// 0x0006.
    pub altitude: Option<Rational>,
    /// 0x0005: 0 above sea level, 1 below.
    pub altitude_ref: Option<u8>,
    /// 0x0007: UTC hours, minutes, seconds.
    pub time_stamp: Option<[Rational; 3]>,
    /// 0x001D: `YYYY:MM:DD`, ASCII up to the first NUL, as stored.
    pub date_stamp: Option<String>,
}

/// ExifTool rounds every rational to 10 significant digits (`%.10g`) before it uses it.
fn round10(x: f64) -> f64 {
    format!("{x:.9e}").parse().unwrap_or(x)
}

/// `d + (m + s / 60) / 60`, the operation order of ExifTool's `GPS::ToDegrees`.
fn dms_degrees(c: &[Rational; 3]) -> Option<f64> {
    let [d, m, s] = c;
    let d = round10(d.to_f64()?);
    let m = round10(m.to_f64()?);
    let s = round10(s.to_f64()?);
    Some(d + (m + s / 60.0) / 60.0)
}

impl Gps {
    /// Latitude in degrees, `d + (m + s/60)/60` with each rational rounded to 10 significant
    /// digits as ExifTool does, negative when the ref is `S`/`s`. `None` when there is no latitude
    /// or one of its rationals has denominator 0.
    pub fn latitude_deg(&self) -> Option<f64> {
        let deg = dms_degrees(&self.latitude?)?;
        Some(
            if self
                .latitude_ref
                .is_some_and(|r| r.eq_ignore_ascii_case(&b'S'))
            {
                -deg
            } else {
                deg
            },
        )
    }

    /// Longitude in degrees, like [`latitude_deg`](Self::latitude_deg); negative when the ref is
    /// `W`/`w`.
    pub fn longitude_deg(&self) -> Option<f64> {
        let deg = dms_degrees(&self.longitude?)?;
        Some(
            if self
                .longitude_ref
                .is_some_and(|r| r.eq_ignore_ascii_case(&b'W'))
            {
                -deg
            } else {
                deg
            },
        )
    }

    /// Altitude in metres, rounded to 10 significant digits; negative when the ref is not 0
    /// (as ExifTool: `-abs`). `None` when there is no altitude or its denominator is 0.
    pub fn altitude_m(&self) -> Option<f64> {
        let a = round10(self.altitude?.to_f64()?);
        Some(if self.altitude_ref.is_some_and(|r| r != 0) {
            -a.abs()
        } else {
            a
        })
    }
}

/// Perl's `\s`: ASCII whitespace including vertical tab.
fn perl_space(c: char) -> bool {
    c.is_ascii_whitespace() || c == '\u{b}'
}

impl Exif {
    /// Parses an EXIF block that starts at the TIFF header (a leading `Exif\0\0` is skipped).
    ///
    /// The walk covers IFD0, IFD1 (linked from IFD0), the Exif and GPS IFDs (IFD0) and the
    /// Interop IFD (Exif IFD). A bad entry (offset outside the block, unknown type) or a bad
    /// sub-IFD offset is skipped; a visited IFD offset is never walked twice.
    ///
    /// # Errors
    ///
    /// [`Error::Malformed`] when the TIFF header or the IFD0 offset is invalid.
    /// [`Error::LimitExceeded`] for `max_exif_bytes` (block length), `max_ifds`, `max_entries`,
    /// `max_total_entries`, `max_depth`, `max_value_bytes` and `max_alloc`.
    /// [`Error::OutOfMemory`]. [`Error::Cancelled`] (checked once per IFD).
    pub fn parse(tiff: &[u8], limits: &Limits, cancel: &AtomicBool) -> Result<Exif, Error> {
        Self::parse_with(tiff, limits, &Budget::new(limits, cancel))
    }

    pub(crate) fn parse_with(
        tiff: &[u8],
        limits: &Limits,
        budget: &Budget<'_>,
    ) -> Result<Exif, Error> {
        crate::tiff::parse(tiff, &[], limits, budget)
    }

    /// [`parse_with`](Self::parse_with) for a block stored in a file: `file` is the file from the
    /// TIFF header on, or empty (when given, `tiff` has no `Exif\0\0` prefix). Only the IFD1
    /// thumbnail may be read from it, when its offset and length end past the block (ExifTool
    /// reads it from the file).
    pub(crate) fn parse_in_file(
        tiff: &[u8],
        file: &[u8],
        limits: &Limits,
        budget: &Budget<'_>,
    ) -> Result<Exif, Error> {
        crate::tiff::parse(tiff, file, limits, budget)
    }

    /// The byte order of the block.
    pub fn byte_order(&self) -> ByteOrder {
        self.order
    }

    /// The entries of one IFD in file order, duplicates kept (empty when the IFD is absent).
    pub fn entries(&self, ifd: IfdKind) -> &[Entry] {
        match self.ifds.get(ifd.index()) {
            Some(v) => v.as_slice(),
            None => &[],
        }
    }

    /// The value of the first entry with that tag.
    pub fn get(&self, ifd: IfdKind, tag: u16) -> Option<&Value> {
        self.entries(ifd)
            .iter()
            .find(|e| e.tag == tag)
            .map(|e| &e.value)
    }

    /// IFD0 Orientation (0x0112), the stored value (first SHORT).
    pub fn orientation(&self) -> Option<u16> {
        match self.get(IfdKind::Ifd0, 0x0112)? {
            Value::Short(v) => v.first().copied(),
            _ => None,
        }
    }

    fn ascii(&self, ifd: IfdKind, tag: u16) -> Option<&str> {
        match self.get(ifd, tag)? {
            Value::Ascii(b) => {
                let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
                std::str::from_utf8(b.get(..end)?).ok()
            }
            _ => None,
        }
    }

    /// IFD0 Make (0x010F), trailing whitespace removed as ExifTool does.
    pub fn make(&self) -> Option<&str> {
        self.ascii(IfdKind::Ifd0, 0x010F)
            .map(|s| s.trim_end_matches(perl_space))
    }

    /// IFD0 Model (0x0110), trailing whitespace removed as ExifTool does.
    pub fn model(&self) -> Option<&str> {
        self.ascii(IfdKind::Ifd0, 0x0110)
            .map(|s| s.trim_end_matches(perl_space))
    }

    /// Exif LensMake (0xA433), as stored (ExifTool does not trim it).
    pub fn lens_make(&self) -> Option<&str> {
        self.ascii(IfdKind::Exif, 0xA433)
    }

    /// Exif LensModel (0xA434), as stored (ExifTool does not trim it).
    pub fn lens_model(&self) -> Option<&str> {
        self.ascii(IfdKind::Exif, 0xA434)
    }

    /// DateTimeOriginal with its sub-second and offset strings; `None` without DateTimeOriginal.
    pub fn capture_time(&self) -> Option<CaptureTime> {
        Some(CaptureTime {
            date_time: self.ascii(IfdKind::Exif, 0x9003)?.to_owned(),
            subsec: self
                .ascii(IfdKind::Exif, 0x9291)
                .map(|s| s.trim_end_matches(' ').to_owned()),
            offset: self.ascii(IfdKind::Exif, 0x9011).map(str::to_owned),
        })
    }

    fn rational(&self, ifd: IfdKind, tag: u16) -> Option<Rational> {
        match self.get(ifd, tag)? {
            Value::Rational(v) => v.first().copied(),
            _ => None,
        }
    }

    fn rationals3(&self, ifd: IfdKind, tag: u16) -> Option<[Rational; 3]> {
        match self.get(ifd, tag)? {
            Value::Rational(v) => match v.as_slice() {
                [a, b, c, ..] => Some([*a, *b, *c]),
                _ => None,
            },
            _ => None,
        }
    }

    fn uint(&self, ifd: IfdKind, tag: u16) -> Option<u32> {
        match self.get(ifd, tag)? {
            Value::Short(v) => v.first().map(|&x| u32::from(x)),
            Value::Long(v) => v.first().copied(),
            _ => None,
        }
    }

    fn first_byte(&self, ifd: IfdKind, tag: u16) -> Option<u8> {
        match self.get(ifd, tag)? {
            Value::Ascii(v) | Value::Byte(v) | Value::Undefined(v) => v.first().copied(),
            _ => None,
        }
    }

    /// Exposure time, f-number, ISO and focal length from the Exif IFD.
    pub fn exposure(&self) -> Exposure {
        Exposure {
            exposure_time: self.rational(IfdKind::Exif, 0x829A),
            f_number: self.rational(IfdKind::Exif, 0x829D),
            iso: self.uint(IfdKind::Exif, 0x8827),
            focal_length: self.rational(IfdKind::Exif, 0x920A),
        }
    }

    /// PixelXDimension (0xA002) and PixelYDimension (0xA003), SHORT or LONG; both must be present.
    pub fn pixel_size(&self) -> Option<(u32, u32)> {
        Some((
            self.uint(IfdKind::Exif, 0xA002)?,
            self.uint(IfdKind::Exif, 0xA003)?,
        ))
    }

    /// The GPS values; `None` when the GPS IFD is absent or has no entries.
    pub fn gps(&self) -> Option<Gps> {
        if self.entries(IfdKind::Gps).is_empty() {
            return None;
        }
        let g = IfdKind::Gps;
        Some(Gps {
            latitude: self.rationals3(g, 0x0002),
            latitude_ref: self.first_byte(g, 0x0001),
            longitude: self.rationals3(g, 0x0004),
            longitude_ref: self.first_byte(g, 0x0003),
            altitude: self.rational(g, 0x0006),
            altitude_ref: self.first_byte(g, 0x0005),
            time_stamp: self.rationals3(g, 0x0007),
            date_stamp: self.ascii(g, 0x001D).map(str::to_owned),
        })
    }

    /// The raw maker note (Exif 0x927C); never parsed.
    pub fn maker_note(&self) -> Option<&[u8]> {
        match self.get(IfdKind::Exif, 0x927C)? {
            Value::Undefined(v) | Value::Byte(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    /// The thumbnail bytes: IFD1 JPEGInterchangeFormat (0x0201) offset and
    /// JPEGInterchangeFormatLength (0x0202) bytes; `None` when either is missing or the range is
    /// outside the block. From [`read`](crate::read) of a JPEG file the range may end past the
    /// APP1 segment (it is then read from the file, as ExifTool does).
    pub fn thumbnail(&self) -> Option<&[u8]> {
        self.thumbnail.as_deref()
    }
}
