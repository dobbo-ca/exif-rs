//! Integration tests for `Exif::parse`: TIFF blocks are built in code, no binary fixtures.

use std::sync::atomic::AtomicBool;

use exif::{ByteOrder, Error, Exif, IfdKind, Limit, Limits, Rational, SRational, Value};

struct Ent {
    tag: u16,
    typ: u16,
    count: u32,
    val: EntVal,
}

enum EntVal {
    /// The value bytes; stored inline when at most 4 bytes, else out of line.
    Data(Vec<u8>),
    /// The 4 bytes of the value/offset field, as given.
    Raw([u8; 4]),
}

/// A TIFF block under construction.
struct T {
    le: bool,
    buf: Vec<u8>,
}

impl T {
    fn new(le: bool) -> T {
        let mut buf = if le {
            b"II*\0".to_vec()
        } else {
            b"MM\0*".to_vec()
        };
        buf.extend([0u8; 4]);
        T { le, buf }
    }

    fn u16(&self, v: u16) -> [u8; 2] {
        if self.le {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }

    fn u32(&self, v: u32) -> [u8; 4] {
        if self.le {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }

    fn u64(&self, v: u64) -> [u8; 8] {
        if self.le {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }

    fn ent(&self, tag: u16, typ: u16, count: u32, data: Vec<u8>) -> Ent {
        Ent {
            tag,
            typ,
            count,
            val: EntVal::Data(data),
        }
    }

    fn raw(&self, tag: u16, typ: u16, count: u32, field: u32) -> Ent {
        Ent {
            tag,
            typ,
            count,
            val: EntVal::Raw(self.u32(field)),
        }
    }

    fn byte(&self, tag: u16, v: &[u8]) -> Ent {
        self.ent(tag, 1, v.len() as u32, v.to_vec())
    }

    fn ascii(&self, tag: u16, s: &str) -> Ent {
        let mut d = s.as_bytes().to_vec();
        d.push(0);
        self.ent(tag, 2, d.len() as u32, d)
    }

    fn short(&self, tag: u16, v: &[u16]) -> Ent {
        self.ent(
            tag,
            3,
            v.len() as u32,
            v.iter().flat_map(|x| self.u16(*x)).collect(),
        )
    }

    fn long(&self, tag: u16, v: &[u32]) -> Ent {
        self.ent(
            tag,
            4,
            v.len() as u32,
            v.iter().flat_map(|x| self.u32(*x)).collect(),
        )
    }

    fn rational(&self, tag: u16, v: &[(u32, u32)]) -> Ent {
        let d = v
            .iter()
            .flat_map(|(n, d)| [self.u32(*n), self.u32(*d)])
            .flatten()
            .collect();
        self.ent(tag, 5, v.len() as u32, d)
    }

    fn undefined(&self, tag: u16, v: &[u8]) -> Ent {
        self.ent(tag, 7, v.len() as u32, v.to_vec())
    }

    /// Appends an IFD (and the out-of-line values of its entries); returns its offset.
    fn put_ifd(&mut self, ents: &[Ent], next: u32) -> u32 {
        if self.buf.len() % 2 == 1 {
            self.buf.push(0);
        }
        let at = self.buf.len();
        let data_pos = at + 2 + 12 * ents.len() + 4;
        let mut head: Vec<u8> = Vec::new();
        let mut tail: Vec<u8> = Vec::new();
        head.extend(self.u16(ents.len() as u16));
        for e in ents {
            head.extend(self.u16(e.tag));
            head.extend(self.u16(e.typ));
            head.extend(self.u32(e.count));
            match &e.val {
                EntVal::Raw(r) => head.extend(r),
                EntVal::Data(d) if d.len() <= 4 => {
                    head.extend(d);
                    head.resize(head.len() + 4 - d.len(), 0);
                }
                EntVal::Data(d) => {
                    head.extend(self.u32((data_pos + tail.len()) as u32));
                    tail.extend(d);
                    if d.len() % 2 == 1 {
                        tail.push(0);
                    }
                }
            }
        }
        head.extend(self.u32(next));
        self.buf.extend(head);
        self.buf.extend(tail);
        at as u32
    }

    /// Appends raw bytes (padded to even length); returns their offset.
    fn put_bytes(&mut self, b: &[u8]) -> u32 {
        if self.buf.len() % 2 == 1 {
            self.buf.push(0);
        }
        let at = self.buf.len();
        self.buf.extend(b);
        at as u32
    }

    fn patch_u16(&mut self, at: usize, v: u16) {
        let b = self.u16(v);
        self.buf[at..at + 2].copy_from_slice(&b);
    }

    fn patch_u32(&mut self, at: usize, v: u32) {
        let b = self.u32(v);
        self.buf[at..at + 4].copy_from_slice(&b);
    }

    fn finish(mut self, ifd0: u32) -> Vec<u8> {
        self.patch_u32(4, ifd0);
        self.buf
    }
}

const THUMB: [u8; 5] = [0xFF, 0xD8, 0xFF, 0xD9, 0x00];

/// A block with every IFD kind: 5 IFDs, 29 entries.
fn sample(le: bool) -> Vec<u8> {
    let mut t = T::new(le);
    let e = [t.ascii(0x0001, "R98")];
    let interop = t.put_ifd(&e, 0);
    let e = [
        t.rational(0x829A, &[(1, 250)]),
        t.rational(0x829D, &[(28, 10)]),
        t.short(0x8827, &[200]),
        t.ascii(0x9003, "2024:05:06 07:08:09"),
        t.ascii(0x9291, "12  "),
        t.ascii(0x9011, "+02:00"),
        t.rational(0x920A, &[(50, 1)]),
        t.undefined(0x927C, &[1, 2, 3, 4, 5, 6]),
        t.long(0xA002, &[4000]),
        t.short(0xA003, &[3000]),
        t.long(0xA005, &[interop]),
        t.ascii(0xA433, "Maker "),
        t.ascii(0xA434, "Lens 24-70 "),
    ];
    let exif = t.put_ifd(&e, 0);
    let e = [
        t.ascii(0x0001, "N"),
        t.rational(0x0002, &[(40, 1), (26, 1), (46, 1)]),
        t.ascii(0x0003, "W"),
        t.rational(0x0004, &[(79, 1), (58, 1), (56, 1)]),
        t.byte(0x0005, &[1]),
        t.rational(0x0006, &[(1234, 10)]),
        t.rational(0x0007, &[(12, 1), (30, 1), (15, 1)]),
        t.ascii(0x001D, "2024:05:06"),
    ];
    let gps = t.put_ifd(&e, 0);
    let thumb = t.put_bytes(&THUMB);
    let e = [
        t.long(0x0201, &[thumb]),
        t.long(0x0202, &[THUMB.len() as u32]),
    ];
    let ifd1 = t.put_ifd(&e, 0);
    let e = [
        t.ascii(0x010F, "Canon  "),
        t.ascii(0x0110, "EOS R5\t "),
        t.short(0x0112, &[6]),
        t.long(0x8769, &[exif]),
        t.long(0x8825, &[gps]),
    ];
    let ifd0 = t.put_ifd(&e, ifd1);
    t.finish(ifd0)
}

/// IFD0 with the given entries.
fn tiff_ifd0(le: bool, f: impl Fn(&T) -> Vec<Ent>) -> Vec<u8> {
    let mut t = T::new(le);
    let e = f(&t);
    let ifd0 = t.put_ifd(&e, 0);
    t.finish(ifd0)
}

/// IFD0 with one pointer tag (0x8769 Exif, 0x8825 GPS) to a sub-IFD with the given entries.
fn tiff_sub(tag: u16, f: impl Fn(&T) -> Vec<Ent>) -> Vec<u8> {
    let mut t = T::new(true);
    let e = f(&t);
    let sub = t.put_ifd(&e, 0);
    let p = [t.long(tag, &[sub])];
    let ifd0 = t.put_ifd(&p, 0);
    t.finish(ifd0)
}

fn lim() -> Limits {
    Limits {
        max_threads: 1,
        ..Limits::default()
    }
}

fn parse(data: &[u8], limits: &Limits) -> Result<Exif, Error> {
    Exif::parse(data, limits, &AtomicBool::new(false))
}

fn tags(x: &Exif, ifd: IfdKind) -> Vec<u16> {
    x.entries(ifd).iter().map(|e| e.tag).collect()
}

fn r10(x: f64) -> f64 {
    format!("{x:.9e}").parse().unwrap()
}

#[test]
fn both_byte_orders() {
    for (le, order) in [(true, ByteOrder::Little), (false, ByteOrder::Big)] {
        let x = parse(&sample(le), &lim()).unwrap();
        assert_eq!(x.byte_order(), order);
        assert_eq!(x.orientation(), Some(6));
        assert_eq!(x.make(), Some("Canon"));
        assert_eq!(x.model(), Some("EOS R5"));
        assert_eq!(x.lens_make(), Some("Maker "));
        assert_eq!(x.lens_model(), Some("Lens 24-70 "));
        let ct = x.capture_time().unwrap();
        assert_eq!(ct.date_time, "2024:05:06 07:08:09");
        assert_eq!(ct.subsec.as_deref(), Some("12"));
        assert_eq!(ct.offset.as_deref(), Some("+02:00"));
        let ex = x.exposure();
        assert_eq!(ex.exposure_time, Some(Rational { num: 1, den: 250 }));
        assert_eq!(ex.f_number, Some(Rational { num: 28, den: 10 }));
        assert_eq!(ex.iso, Some(200));
        assert_eq!(ex.focal_length, Some(Rational { num: 50, den: 1 }));
        assert_eq!(x.pixel_size(), Some((4000, 3000)));
        assert_eq!(x.maker_note(), Some(&[1u8, 2, 3, 4, 5, 6][..]));
        assert_eq!(x.thumbnail(), Some(&THUMB[..]));
        let g = x.gps().unwrap();
        assert_eq!(g.latitude_ref, Some(b'N'));
        assert_eq!(g.longitude_ref, Some(b'W'));
        assert_eq!(g.altitude_ref, Some(1));
        assert_eq!(g.date_stamp.as_deref(), Some("2024:05:06"));
        assert_eq!(
            g.time_stamp,
            Some([
                Rational { num: 12, den: 1 },
                Rational { num: 30, den: 1 },
                Rational { num: 15, den: 1 }
            ])
        );
        assert_eq!(g.latitude_deg(), Some(40.0 + (26.0 + 46.0 / 60.0) / 60.0));
        assert_eq!(
            g.longitude_deg(),
            Some(-(79.0 + (58.0 + 56.0 / 60.0) / 60.0))
        );
        assert_eq!(g.altitude_m(), Some(-123.4));
    }
}

#[test]
fn sub_ifds_and_thumbnail_entries() {
    let x = parse(&sample(true), &lim()).unwrap();
    assert_eq!(
        tags(&x, IfdKind::Ifd0),
        [0x010F, 0x0110, 0x0112, 0x8769, 0x8825]
    );
    assert_eq!(x.entries(IfdKind::Exif).len(), 13);
    assert_eq!(x.entries(IfdKind::Gps).len(), 8);
    assert_eq!(tags(&x, IfdKind::Interop), [0x0001]);
    assert_eq!(tags(&x, IfdKind::Ifd1), [0x0201, 0x0202]);
    assert_eq!(
        x.get(IfdKind::Interop, 0x0001),
        Some(&Value::Ascii(b"R98\0".to_vec()))
    );
    assert_eq!(x.get(IfdKind::Ifd0, 0x9999), None);
}

#[test]
fn exif_prefix_is_skipped() {
    let data = sample(false);
    let mut prefixed = b"Exif\0\0".to_vec();
    prefixed.extend(&data);
    assert_eq!(
        parse(&prefixed, &lim()).unwrap(),
        parse(&data, &lim()).unwrap()
    );
}

#[test]
fn bad_header_is_malformed() {
    let bad: [&[u8]; 5] = [
        b"",
        b"II*\0",
        b"XX*\0\x08\0\0\0",
        b"I\0*\0\x08\0\0\0",
        b"Exif\0\0",
    ];
    for b in bad {
        assert_eq!(
            parse(b, &lim()).unwrap_err(),
            Error::Malformed("tiff header"),
            "{b:?}"
        );
    }
    // IFD0 offset outside the block, or inside the header.
    for off in [8u32, 100, 4] {
        let mut d = b"II*\0".to_vec();
        d.extend(off.to_le_bytes());
        assert_eq!(
            parse(&d, &lim()).unwrap_err(),
            Error::Malformed("ifd0 offset")
        );
    }
}

#[test]
fn every_value_type() {
    for le in [true, false] {
        let data = tiff_ifd0(le, |t| {
            vec![
                t.byte(0x2001, &[1, 2, 3, 4, 5]),
                t.ascii(0x2002, "hello"),
                t.short(0x2003, &[1, 2, 3]),
                t.long(0x2004, &[70000, 5]),
                t.rational(0x2005, &[(1, 2), (3, 4)]),
                t.ent(0x2006, 6, 5, vec![0xFF, 2, 0xFD, 4, 0xFB]),
                t.undefined(0x2007, &[9, 8, 7, 6, 5]),
                t.ent(
                    0x2008,
                    8,
                    3,
                    [-2i16, 3, -4]
                        .iter()
                        .flat_map(|v| t.u16(*v as u16))
                        .collect(),
                ),
                t.ent(
                    0x2009,
                    9,
                    2,
                    [-70000i32, 1]
                        .iter()
                        .flat_map(|v| t.u32(*v as u32))
                        .collect(),
                ),
                t.ent(
                    0x200A,
                    10,
                    2,
                    [-1i32, 2, 3, -4]
                        .iter()
                        .flat_map(|v| t.u32(*v as u32))
                        .collect(),
                ),
                t.ent(
                    0x200B,
                    11,
                    2,
                    [1.5f32, -2.25]
                        .iter()
                        .flat_map(|v| t.u32(v.to_bits()))
                        .collect(),
                ),
                t.ent(0x200C, 12, 1, t.u64(3.5f64.to_bits()).to_vec()),
                t.ent(0x200D, 13, 1, t.u32(99).to_vec()),
                t.byte(0x200E, &[7]),
                t.short(0x200F, &[5]),
                t.ent(0x2010, 6, 1, vec![0xFE]),
            ]
        });
        let x = parse(&data, &lim()).unwrap();
        let get = |tag| x.get(IfdKind::Ifd0, tag).cloned();
        assert_eq!(get(0x2001), Some(Value::Byte(vec![1, 2, 3, 4, 5])));
        assert_eq!(get(0x2002), Some(Value::Ascii(b"hello\0".to_vec())));
        assert_eq!(get(0x2003), Some(Value::Short(vec![1, 2, 3])));
        assert_eq!(get(0x2004), Some(Value::Long(vec![70000, 5])));
        assert_eq!(
            get(0x2005),
            Some(Value::Rational(vec![
                Rational { num: 1, den: 2 },
                Rational { num: 3, den: 4 }
            ]))
        );
        assert_eq!(get(0x2006), Some(Value::SByte(vec![-1, 2, -3, 4, -5])));
        assert_eq!(get(0x2007), Some(Value::Undefined(vec![9, 8, 7, 6, 5])));
        assert_eq!(get(0x2008), Some(Value::SShort(vec![-2, 3, -4])));
        assert_eq!(get(0x2009), Some(Value::SLong(vec![-70000, 1])));
        assert_eq!(
            get(0x200A),
            Some(Value::SRational(vec![
                SRational { num: -1, den: 2 },
                SRational { num: 3, den: -4 }
            ]))
        );
        assert_eq!(get(0x200B), Some(Value::Float(vec![1.5, -2.25])));
        assert_eq!(get(0x200C), Some(Value::Double(vec![3.5])));
        assert_eq!(get(0x200D), Some(Value::Long(vec![99])));
        assert_eq!(get(0x200E), Some(Value::Byte(vec![7])));
        assert_eq!(get(0x200F), Some(Value::Short(vec![5])));
        assert_eq!(get(0x2010), Some(Value::SByte(vec![-2])));
    }
}

#[test]
fn file_order_and_duplicates_are_kept() {
    let data = tiff_ifd0(true, |t| {
        vec![
            t.short(0x0112, &[3]),
            t.short(0x0100, &[1]),
            t.short(0x0112, &[5]),
        ]
    });
    let x = parse(&data, &lim()).unwrap();
    assert_eq!(tags(&x, IfdKind::Ifd0), [0x0112, 0x0100, 0x0112]);
    assert_eq!(x.get(IfdKind::Ifd0, 0x0112), Some(&Value::Short(vec![3])));
    assert_eq!(x.orientation(), Some(3));
}

#[test]
fn orientation_is_the_first_integer_entry() {
    // ExifTool reads any integer type (no Format for 0x0112).
    let data = tiff_ifd0(true, |t| vec![t.long(0x0112, &[6])]);
    assert_eq!(parse(&data, &lim()).unwrap().orientation(), Some(6));
    let data = tiff_ifd0(false, |t| vec![t.short(0x0112, &[8])]);
    assert_eq!(parse(&data, &lim()).unwrap().orientation(), Some(8));
    let data = tiff_ifd0(true, |t| vec![t.byte(0x0112, &[3])]);
    assert_eq!(parse(&data, &lim()).unwrap().orientation(), Some(3));
    // Not an integer, or out of range: skipped.
    let data = tiff_ifd0(true, |t| {
        vec![
            t.ascii(0x0112, "6"),
            t.long(0x0112, &[70_000]),
            t.ent(0x0112, 8, 1, (-1i16).to_le_bytes().to_vec()),
            t.short(0x0112, &[5]),
        ]
    });
    assert_eq!(parse(&data, &lim()).unwrap().orientation(), Some(5));
}

#[test]
fn accessors_read_the_last_of_duplicate_tags() {
    // ExifTool keeps the last of two priority-1 tags (Make, LensModel, ISO, GPS ...).
    let data = tiff_ifd0(true, |t| {
        vec![
            t.ascii(0x010F, "Canon"),
            t.short(0x0112, &[3]),
            t.ascii(0x010F, "Nikon"),
            t.short(0x0112, &[6]),
        ]
    });
    let x = parse(&data, &lim()).unwrap();
    assert_eq!(x.make(), Some("Nikon"));
    assert_eq!(x.orientation(), Some(3));
    let data = tiff_sub(0x8769, |t| {
        vec![
            t.short(0x8827, &[100]),
            t.ascii(0xA434, "A"),
            t.short(0x8827, &[200]),
            t.ascii(0xA434, "B"),
        ]
    });
    let x = parse(&data, &lim()).unwrap();
    assert_eq!(x.exposure().iso, Some(200));
    assert_eq!(x.lens_model(), Some("B"));
    let data = tiff_sub(0x8825, |t| vec![t.ascii(0x0001, "N"), t.ascii(0x0001, "S")]);
    assert_eq!(
        parse(&data, &lim()).unwrap().gps().unwrap().latitude_ref,
        Some(b'S')
    );
}

#[test]
fn utf8_type_129_is_read_as_text() {
    let data = tiff_ifd0(true, |t| vec![t.ent(0x010F, 129, 6, b"Canon\0".to_vec())]);
    let x = parse(&data, &lim()).unwrap();
    assert_eq!(x.make(), Some("Canon"));
    assert_eq!(
        x.get(IfdKind::Ifd0, 0x010F),
        Some(&Value::Ascii(b"Canon\0".to_vec()))
    );
}

#[test]
fn ifd0_next_pointer_to_itself() {
    let mut t = T::new(true);
    let e = [t.short(0x0112, &[1])];
    let ifd0 = t.put_ifd(&e, 8);
    assert_eq!(ifd0, 8);
    let x = parse(&t.finish(ifd0), &lim()).unwrap();
    assert_eq!(x.entries(IfdKind::Ifd0).len(), 1);
    assert!(x.entries(IfdKind::Ifd1).is_empty());
}

#[test]
fn ifd1_pointing_back_to_ifd0_stops() {
    let mut t = T::new(false);
    let e0 = [t.short(0x0112, &[1])];
    let ifd0 = t.put_ifd(&e0, 0);
    let e1 = [t.short(0x0112, &[2])];
    let ifd1 = t.put_ifd(&e1, ifd0);
    t.patch_u32(ifd0 as usize + 2 + 12, ifd1);
    let x = parse(&t.finish(ifd0), &lim()).unwrap();
    assert_eq!(x.entries(IfdKind::Ifd0).len(), 1);
    assert_eq!(x.get(IfdKind::Ifd1, 0x0112), Some(&Value::Short(vec![2])));
}

#[test]
fn sub_ifd_pointers_to_visited_ifds_are_skipped() {
    // IFD0 -> Exif at itself, GPS at itself.
    let data = tiff_ifd0(true, |t| vec![t.long(0x8769, &[8]), t.long(0x8825, &[8])]);
    let x = parse(&data, &lim()).unwrap();
    assert_eq!(x.entries(IfdKind::Ifd0).len(), 2);
    assert!(x.entries(IfdKind::Exif).is_empty());
    assert!(x.entries(IfdKind::Gps).is_empty());

    // IFD0 at 8 (1 entry, 18 bytes), Exif IFD at 26: its Interop pointer and its next pointer
    // both point back to IFD0.
    let mut t = T::new(true);
    let e0 = [t.long(0x8769, &[26])];
    let ifd0 = t.put_ifd(&e0, 0);
    let e1 = [t.long(0xA005, &[8])];
    let exif = t.put_ifd(&e1, 8);
    assert_eq!((ifd0, exif), (8, 26));
    let x = parse(&t.finish(ifd0), &lim()).unwrap();
    assert_eq!(tags(&x, IfdKind::Exif), [0xA005]);
    assert!(x.entries(IfdKind::Interop).is_empty());

    // Exif and GPS pointers to the same IFD: the second is skipped.
    let mut t = T::new(true);
    let e = [t.short(0x0100, &[1])];
    let sub = t.put_ifd(&e, 0);
    let e0 = [t.long(0x8769, &[sub]), t.long(0x8825, &[sub])];
    let ifd0 = t.put_ifd(&e0, 0);
    let x = parse(&t.finish(ifd0), &lim()).unwrap();
    assert_eq!(x.entries(IfdKind::Exif).len(), 1);
    assert!(x.entries(IfdKind::Gps).is_empty());
}

#[test]
fn shared_offsets_go_to_the_ifd_exiftool_walks_first() {
    // ExifTool walks a sub-IFD when it meets the pointer entry and IFD1 after IFD0's entries.
    // IFD0 next pointer == Exif IFD offset: the IFD is the Exif IFD, IFD1 is empty.
    let mut t = T::new(true);
    let e0 = [t.long(0x8769, &[26])];
    let ifd0 = t.put_ifd(&e0, 26);
    let e1 = [t.ascii(0x9003, "2020:01:02 03:04:05")];
    let exif = t.put_ifd(&e1, 0);
    assert_eq!((ifd0, exif), (8, 26));
    let x = parse(&t.finish(ifd0), &lim()).unwrap();
    assert_eq!(tags(&x, IfdKind::Exif), [0x9003]);
    assert!(x.entries(IfdKind::Ifd1).is_empty());
    assert_eq!(
        x.capture_time().map(|c| c.date_time),
        Some("2020:01:02 03:04:05".to_string())
    );

    // Interop pointer == IFD1 offset: the Interop IFD has it, so there is no thumbnail.
    let mut t = T::new(true);
    let thumb = t.put_bytes(&THUMB);
    let e = [
        t.long(0x0201, &[thumb]),
        t.long(0x0202, &[THUMB.len() as u32]),
    ];
    let shared = t.put_ifd(&e, 0);
    let e = [t.long(0xA005, &[shared])];
    let exif = t.put_ifd(&e, 0);
    let e0 = [t.long(0x8769, &[exif])];
    let ifd0 = t.put_ifd(&e0, shared);
    let x = parse(&t.finish(ifd0), &lim()).unwrap();
    assert_eq!(tags(&x, IfdKind::Interop), [0x0201, 0x0202]);
    assert!(x.entries(IfdKind::Ifd1).is_empty());
    assert_eq!(x.thumbnail(), None);

    // GPS listed before Exif, both at one offset: GPS has it.
    let mut t = T::new(true);
    let e = [t.short(0x0100, &[1])];
    let sub = t.put_ifd(&e, 0);
    let e0 = [t.long(0x8825, &[sub]), t.long(0x8769, &[sub])];
    let ifd0 = t.put_ifd(&e0, 0);
    let x = parse(&t.finish(ifd0), &lim()).unwrap();
    assert_eq!(x.entries(IfdKind::Gps).len(), 1);
    assert!(x.entries(IfdKind::Exif).is_empty());

    // GPS at the Interop IFD's offset is read again as GPS (ExifTool's Windows Phone patch).
    let mut t = T::new(true);
    let e = [t.ascii(0x0001, "N")];
    let shared = t.put_ifd(&e, 0);
    let e = [t.long(0xA005, &[shared])];
    let exif = t.put_ifd(&e, 0);
    let e0 = [t.long(0x8769, &[exif]), t.long(0x8825, &[shared])];
    let ifd0 = t.put_ifd(&e0, 0);
    let x = parse(&t.finish(ifd0), &lim()).unwrap();
    assert_eq!(tags(&x, IfdKind::Interop), [0x0001]);
    assert_eq!(tags(&x, IfdKind::Gps), [0x0001]);
}

#[test]
fn bad_entries_and_offsets_are_skipped() {
    let data = tiff_ifd0(true, |t| {
        vec![
            t.raw(0x0100, 2, 100, 0xFFFF_0000),
            t.raw(0x0101, 4, 0xFFFF_FFFF, 8),
            t.raw(0x0102, 99, 1, 0),
            t.raw(0x0103, 0, 1, 0),
            t.raw(0x0104, 5, 0x2000_0000, 0xFFFF_FFF0),
            t.short(0x0112, &[7]),
            t.raw(0x8769, 4, 1, 0x00FF_FFFF),
            t.raw(0x8825, 4, 1, 3),
        ]
    });
    // A tiny allocation budget must not matter: skipped entries allocate nothing.
    let limits = Limits {
        max_alloc: 4096,
        ..lim()
    };
    let x = parse(&data, &limits).unwrap();
    assert_eq!(tags(&x, IfdKind::Ifd0), [0x0112, 0x8769, 0x8825]);
    assert!(x.entries(IfdKind::Exif).is_empty());
    assert!(x.entries(IfdKind::Gps).is_empty());
}

#[test]
fn ifd_longer_than_the_block_gives_the_entries_that_fit() {
    let mut t = T::new(true);
    let e = [t.short(0x0100, &[1]), t.short(0x0101, &[2])];
    let ifd0 = t.put_ifd(&e, 0);
    t.patch_u16(ifd0 as usize, 100);
    let x = parse(&t.finish(ifd0), &lim()).unwrap();
    assert_eq!(tags(&x, IfdKind::Ifd0), [0x0100, 0x0101]);
}

#[test]
fn thumbnail_outside_the_block_is_none() {
    let data = tiff_ifd0(true, |t| {
        vec![t.long(0x0201, &[10_000]), t.long(0x0202, &[16])]
    });
    let mut t = T::new(true);
    let e0 = [t.short(0x0112, &[1])];
    let ifd0 = t.put_ifd(&e0, 0);
    let e1 = [t.long(0x0201, &[10_000]), t.long(0x0202, &[16])];
    let ifd1 = t.put_ifd(&e1, 0);
    t.patch_u32(ifd0 as usize + 2 + 12, ifd1);
    let x = parse(&t.finish(ifd0), &lim()).unwrap();
    assert_eq!(x.entries(IfdKind::Ifd1).len(), 2);
    assert_eq!(x.thumbnail(), None);
    // Thumbnail tags in IFD0 are not a thumbnail.
    assert_eq!(parse(&data, &lim()).unwrap().thumbnail(), None);
}

#[test]
fn maker_note_is_raw_bytes() {
    let note: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
    let data = tiff_sub(0x8769, |t| vec![t.undefined(0x927C, &note)]);
    let x = parse(&data, &lim()).unwrap();
    assert_eq!(x.maker_note(), Some(&note[..]));
}

#[test]
fn string_accessors_follow_exiftool() {
    let data = tiff_ifd0(true, |t| {
        vec![
            t.ent(0x010F, 2, 9, b"  Cam \t\n\0".to_vec()),
            t.ent(0x0110, 2, 8, b"Ab\0cd \0\0".to_vec()),
        ]
    });
    let x = parse(&data, &lim()).unwrap();
    assert_eq!(x.make(), Some("  Cam"));
    assert_eq!(x.model(), Some("Ab"));

    let data = tiff_ifd0(true, |t| {
        vec![t.ent(0x010F, 2, 2, vec![0xFF, 0]), t.short(0x0110, &[1])]
    });
    let x = parse(&data, &lim()).unwrap();
    assert_eq!(x.make(), None);
    assert_eq!(x.model(), None);

    // No terminating NUL: the whole value.
    let data = tiff_ifd0(false, |t| vec![t.ent(0x010F, 2, 5, b"Nikon".to_vec())]);
    assert_eq!(parse(&data, &lim()).unwrap().make(), Some("Nikon"));
}

#[test]
fn capture_time_needs_date_time_original() {
    let data = tiff_sub(0x8769, |t| vec![t.ascii(0x9291, "5"), t.ascii(0x9011, "Z")]);
    assert_eq!(parse(&data, &lim()).unwrap().capture_time(), None);
    let data = tiff_sub(0x8769, |t| vec![t.ascii(0x9003, "2000:01:02 03:04:05")]);
    let ct = parse(&data, &lim()).unwrap().capture_time().unwrap();
    assert_eq!((ct.subsec, ct.offset), (None, None));
}

#[test]
fn exposure_and_pixel_size_variants() {
    let data = tiff_sub(0x8769, |t| {
        vec![
            t.long(0x8827, &[64000, 1]),
            t.long(0xA002, &[10]),
            t.short(0xA003, &[20]),
        ]
    });
    let x = parse(&data, &lim()).unwrap();
    assert_eq!(x.exposure().iso, Some(64000));
    assert_eq!(x.exposure().exposure_time, None);
    assert_eq!(x.pixel_size(), Some((10, 20)));
    let data = tiff_sub(0x8769, |t| vec![t.short(0xA002, &[10])]);
    assert_eq!(parse(&data, &lim()).unwrap().pixel_size(), None);
}

#[test]
fn gps_degrees_follow_exiftool_operation_order() {
    let data = tiff_sub(0x8825, |t| {
        vec![
            t.ascii(0x0001, "S"),
            t.rational(0x0002, &[(40, 1), (26, 1), (46, 1)]),
            t.ascii(0x0003, "E"),
            t.rational(0x0004, &[(1, 3), (0, 1), (0, 1)]),
            t.byte(0x0005, &[0]),
            t.rational(0x0006, &[(1234, 10)]),
        ]
    });
    let g = parse(&data, &lim()).unwrap().gps().unwrap();
    assert_eq!(
        g.latitude_deg(),
        Some(-(40.0 + (26.0 + 46.0 / 60.0) / 60.0))
    );
    // Each rational is rounded to 10 significant digits first, as ExifTool prints it.
    assert_ne!(r10(1.0 / 3.0), 1.0 / 3.0);
    assert_eq!(
        g.longitude_deg(),
        Some(r10(1.0 / 3.0) + (0.0 + 0.0 / 60.0) / 60.0)
    );
    assert_eq!(g.altitude_m(), Some(123.4));
}

#[test]
fn gps_edge_cases() {
    // Denominator 0 anywhere: no value.
    let data = tiff_sub(0x8825, |t| {
        vec![
            t.ascii(0x0001, "N"),
            t.rational(0x0002, &[(1, 1), (2, 0), (3, 1)]),
            t.rational(0x0006, &[(5, 0)]),
            t.byte(0x0005, &[1]),
        ]
    });
    let g = parse(&data, &lim()).unwrap().gps().unwrap();
    assert_eq!(g.latitude_deg(), None);
    assert_eq!(g.longitude_deg(), None);
    assert_eq!(g.altitude_m(), None);
    assert_eq!(Rational { num: 1, den: 0 }.to_f64(), None);
    assert_eq!(Rational { num: 0, den: 0 }.to_f64(), None);
    assert_eq!(Rational { num: 1, den: 4 }.to_f64(), Some(0.25));

    // No ref: positive. Lower case ref: also negative. Any non-zero altitude ref: below.
    let data = tiff_sub(0x8825, |t| {
        vec![
            t.rational(0x0002, &[(10, 1), (30, 1), (0, 1)]),
            t.ascii(0x0003, "w"),
            t.rational(0x0004, &[(10, 1), (0, 1), (0, 1)]),
            t.rational(0x0006, &[(7, 1)]),
            t.byte(0x0005, &[3]),
        ]
    });
    let g = parse(&data, &lim()).unwrap().gps().unwrap();
    assert_eq!(g.latitude_deg(), Some(10.5));
    assert_eq!(g.longitude_deg(), Some(-10.0));
    assert_eq!(g.altitude_m(), Some(-7.0));

    // No GPS IFD, or an empty one.
    assert_eq!(
        parse(&tiff_ifd0(true, |t| vec![t.short(0x0112, &[1])]), &lim())
            .unwrap()
            .gps(),
        None
    );
    assert_eq!(
        parse(&tiff_sub(0x8825, |_| vec![]), &lim()).unwrap().gps(),
        None
    );
}

#[test]
fn limit_ifds() {
    let data = sample(true);
    assert!(
        parse(
            &data,
            &Limits {
                max_ifds: 5,
                ..lim()
            }
        )
        .is_ok()
    );
    assert_eq!(
        parse(
            &data,
            &Limits {
                max_ifds: 4,
                ..lim()
            }
        ),
        Err(Error::LimitExceeded(Limit::Ifds))
    );
}

#[test]
fn limit_entries_is_per_ifd() {
    let data = sample(true);
    // The Exif IFD has 13 entries, the largest.
    assert!(
        parse(
            &data,
            &Limits {
                max_entries: 13,
                ..lim()
            }
        )
        .is_ok()
    );
    assert_eq!(
        parse(
            &data,
            &Limits {
                max_entries: 12,
                ..lim()
            }
        ),
        Err(Error::LimitExceeded(Limit::Entries))
    );
}

#[test]
fn limit_total_entries() {
    let data = sample(false);
    let x = parse(&data, &lim()).unwrap();
    let total: usize = [
        IfdKind::Ifd0,
        IfdKind::Exif,
        IfdKind::Gps,
        IfdKind::Interop,
        IfdKind::Ifd1,
    ]
    .iter()
    .map(|&k| x.entries(k).len())
    .sum();
    assert_eq!(total, 29);
    assert!(
        parse(
            &data,
            &Limits {
                max_total_entries: total,
                ..lim()
            }
        )
        .is_ok()
    );
    assert_eq!(
        parse(
            &data,
            &Limits {
                max_total_entries: total - 1,
                ..lim()
            }
        ),
        Err(Error::LimitExceeded(Limit::TotalEntries))
    );
}

#[test]
fn limit_depth() {
    let data = sample(true);
    assert!(
        parse(
            &data,
            &Limits {
                max_depth: 2,
                ..lim()
            }
        )
        .is_ok()
    );
    assert_eq!(
        parse(
            &data,
            &Limits {
                max_depth: 1,
                ..lim()
            }
        ),
        Err(Error::LimitExceeded(Limit::Depth))
    );
    // IFD0 and IFD1 are depth 0; a sub-IFD is depth 1.
    assert_eq!(
        parse(
            &sample(true),
            &Limits {
                max_depth: 0,
                ..lim()
            }
        ),
        Err(Error::LimitExceeded(Limit::Depth))
    );
    let mut t = T::new(true);
    let e0 = [t.short(0x0112, &[1])];
    let ifd0 = t.put_ifd(&e0, 0);
    let e1 = [t.short(0x0112, &[2])];
    let ifd1 = t.put_ifd(&e1, 0);
    t.patch_u32(ifd0 as usize + 2 + 12, ifd1);
    let data = t.finish(ifd0);
    assert!(
        parse(
            &data,
            &Limits {
                max_depth: 0,
                ..lim()
            }
        )
        .is_ok()
    );
}

#[test]
fn limit_value_bytes() {
    let data = sample(true);
    assert!(parse(&data, &lim()).is_ok());
    assert_eq!(
        parse(
            &data,
            &Limits {
                max_value_bytes: 10,
                ..lim()
            }
        ),
        Err(Error::LimitExceeded(Limit::ValueBytes))
    );
    // Value regions never overlap, so their sum is at most the block length.
    assert!(
        parse(
            &data,
            &Limits {
                max_value_bytes: data.len(),
                ..lim()
            }
        )
        .is_ok()
    );
    assert_eq!(
        parse(
            &data,
            &Limits {
                max_value_bytes: 29,
                ..lim()
            }
        ),
        Err(Error::LimitExceeded(Limit::ValueBytes))
    );
}

#[test]
fn limit_exif_bytes() {
    let data = sample(true);
    assert!(
        parse(
            &data,
            &Limits {
                max_exif_bytes: data.len(),
                ..lim()
            }
        )
        .is_ok()
    );
    assert_eq!(
        parse(
            &data,
            &Limits {
                max_exif_bytes: data.len() - 1,
                ..lim()
            }
        ),
        Err(Error::LimitExceeded(Limit::ExifBytes))
    );
    // Checked before the header: an oversize block of garbage is a limit error too.
    assert_eq!(
        parse(
            &[0u8; 64],
            &Limits {
                max_exif_bytes: 63,
                ..lim()
            }
        ),
        Err(Error::LimitExceeded(Limit::ExifBytes))
    );
}

#[test]
fn limit_alloc() {
    let data = sample(true);
    assert_eq!(
        parse(
            &data,
            &Limits {
                max_alloc: 64,
                ..lim()
            }
        ),
        Err(Error::LimitExceeded(Limit::Alloc))
    );
    assert!(
        parse(
            &data,
            &Limits {
                max_alloc: 1 << 20,
                ..lim()
            }
        )
        .is_ok()
    );
}

#[test]
fn cancel_flag_set_before_the_call() {
    let cancel = AtomicBool::new(true);
    assert_eq!(
        Exif::parse(&sample(true), &lim(), &cancel),
        Err(Error::Cancelled)
    );
}

#[test]
fn truncated_and_corrupted_blocks_never_panic() {
    for le in [true, false] {
        let data = sample(le);
        let mut prefixed = b"Exif\0\0".to_vec();
        prefixed.extend(&data);
        for d in [&data, &prefixed] {
            for n in 0..=d.len() {
                let _ = parse(&d[..n], &lim());
            }
            for i in 0..d.len() {
                for v in [0x00u8, 0xFF, 0x7F] {
                    let mut c = d.clone();
                    c[i] = v;
                    let _ = parse(&c, &lim());
                }
            }
            assert!(parse(d, &lim()).is_ok());
        }
    }
}
