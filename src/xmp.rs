//! XMP packets: a small tolerant XML reader that extracts the simple properties.
//!
//! The reader has no DTD and no external entities. ExifTool 13.64 is the behavioural reference
//! (no code is copied): element text is kept as stored (not trimmed; ExifTool trims only
//! `rdf:Description` text), entities and CDATA are resolved, comments inside a value are removed.

use std::ops::{ControlFlow, Range};
use std::sync::atomic::AtomicBool;

use crate::budget::Budget;
use crate::{Error, Limit, Limits};

const NS_RDF: &[u8] = b"http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const NS_TIFF: &str = "http://ns.adobe.com/tiff/1.0/";
const NS_XMP: &str = "http://ns.adobe.com/xap/1.0/";
const NS_HDRGM: &str = "http://ns.adobe.com/hdr-gain-map/1.0/";
/// Most `xmlns` declarations in scope at once; a deeper stack ends the scan (real XMP has ~20).
const MAX_NS: usize = 256;
/// How many open elements a closing tag looks through for its opening tag.
const CLOSE_LOOKBACK: usize = 16;

/// One simple XMP property.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Property {
    /// The namespace URI (not the prefix), for example `http://ns.adobe.com/tiff/1.0/`.
    pub ns: String,
    /// The local name, for example `Orientation`.
    pub name: String,
    /// The text, with entities and CDATA resolved.
    pub value: String,
    /// Whether this is a top-level property: an attribute or child element of an
    /// `rdf:Description` that is not inside another property, or an item of such a property's
    /// `rdf:Seq`/`rdf:Bag`/`rdf:Alt`. A field of a structure (for example an `xmpMM:Pantry` item
    /// or `exif:Flash/exif:Fired`) is not: ExifTool gives it a flattened name.
    pub top_level: bool,
}

/// A parsed XMP packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Xmp {
    raw: Vec<u8>,
    props: Vec<Property>,
}

impl Xmp {
    /// Parses an XMP packet (and the extended packet of a JPEG, when there is one).
    ///
    /// A property is simple when it is an attribute of `rdf:Description` (other than `rdf:*`,
    /// `xml:*` and `xmlns:*`) or an element with text only inside a description or a structure.
    /// Each `rdf:li` text of `rdf:Seq`, `rdf:Bag` and `rdf:Alt` is one property named after the
    /// array. Properties of the extended packet come after those of the standard one. A property
    /// whose text is not valid UTF-8 is left out. Malformed XML after the start gives the
    /// properties read so far. An unterminated start tag, comment or CDATA section ends the
    /// scan; so does a stack of more than 256 `xmlns` declarations. Empty self-closing elements
    /// give no property. The packet must be UTF-8 (UTF-16 gives [`Error::Malformed`]).
    ///
    /// # Errors
    ///
    /// [`Error::LimitExceeded`] for `max_xmp_bytes` (standard and extended together),
    /// `max_xmp_nodes` (elements and attributes, per packet) and `max_alloc`.
    /// [`Error::Malformed`] when the packet does not start with `<` after an optional UTF-8 BOM
    /// and whitespace. [`Error::OutOfMemory`]. [`Error::Cancelled`] (checked every 1024 nodes).
    pub fn parse(
        packet: &[u8],
        extended: Option<&[u8]>,
        limits: &Limits,
        cancel: &AtomicBool,
    ) -> Result<Xmp, Error> {
        Self::parse_with(packet, extended, limits, &Budget::new(limits, cancel))
    }

    pub(crate) fn parse_with(
        packet: &[u8],
        extended: Option<&[u8]>,
        limits: &Limits,
        budget: &Budget<'_>,
    ) -> Result<Xmp, Error> {
        let total = packet.len().saturating_add(extended.map_or(0, <[u8]>::len));
        if total > limits.max_xmp_bytes {
            return Err(Error::LimitExceeded(Limit::XmpBytes));
        }
        budget.check_cancel()?;
        if packet.get(body_start(packet)) != Some(&b'<') {
            return Err(Error::Malformed("xmp"));
        }
        let raw = budget.copy(packet)?;

        // Count first so that the property vector is reserved once, at its final size.
        let mut n = 0usize;
        for data in std::iter::once(packet).chain(extended) {
            scan(data, limits, budget, &mut |_, _, _, _| {
                n += 1;
                Ok(ControlFlow::Continue(()))
            })?;
        }
        let mut props = budget.vec::<Property>(n)?;
        for data in std::iter::once(packet).chain(extended) {
            collect(data, limits, budget, &mut props)?;
        }
        Ok(Xmp { raw, props })
    }

    /// The standard packet as stored.
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// The simple properties in document order, those of the extended packet last.
    pub fn properties(&self) -> &[Property] {
        &self.props
    }

    /// The texts of the top-level properties with that namespace URI and local name, in
    /// document order.
    fn values<'a>(&'a self, ns: &str, name: &str) -> impl DoubleEndedIterator<Item = &'a str> {
        self.props
            .iter()
            .filter(move |p| p.top_level && p.ns == ns && p.name == name)
            .map(|p| p.value.as_str())
    }

    /// The text of the first top-level property with that namespace URI and local name
    /// (structure fields are not looked at).
    pub fn get(&self, ns: &str, name: &str) -> Option<&str> {
        self.values(ns, name).next()
    }

    /// The first top-level `tiff:Orientation` (`http://ns.adobe.com/tiff/1.0/`) as a number
    /// (ExifTool's priority 0: the first one stays); surrounding whitespace is ignored. `None`
    /// when absent or not a `u16`. ExifTool prints the integer.
    pub fn orientation(&self) -> Option<u16> {
        self.get(NS_TIFF, "Orientation")?.trim().parse().ok()
    }

    /// The first top-level `xmp:CreateDate` (`http://ns.adobe.com/xap/1.0/`; priority 0, as
    /// ExifTool), the raw text as stored (for example `2020-01-02T03:04:05Z`; ExifTool rewrites
    /// the date to `2020:01:02 03:04:05Z`).
    pub fn create_date(&self) -> Option<&str> {
        self.get(NS_XMP, "CreateDate")
    }

    /// The last top-level `xmp:Rating` (ExifTool keeps the last one), the raw text as stored
    /// (ExifTool prints it as a number).
    pub fn rating(&self) -> Option<&str> {
        self.values(NS_XMP, "Rating").next_back()
    }

    /// The top-level properties of the HDR gain map namespace
    /// `http://ns.adobe.com/hdr-gain-map/1.0/` (`hdrgm:Version`, `hdrgm:GainMapMax`, ...), raw
    /// text.
    pub fn hdrgm(&self) -> impl Iterator<Item = &Property> {
        self.props
            .iter()
            .filter(|p| p.top_level && p.ns == NS_HDRGM)
    }
}

/// The byte range in `packet` of the raw text of the first top-level `tiff:Orientation` value
/// (the attribute value without quotes, or the element text); `None` when absent. The scan is
/// charged to `budget` and checks its cancel flag.
///
/// # Errors
///
/// As [`Xmp::parse`]: `LimitExceeded` for `max_xmp_bytes`, `max_xmp_nodes` and `max_alloc`,
/// `OutOfMemory`, `Cancelled`.
pub(crate) fn orientation_value_span(
    packet: &[u8],
    limits: &Limits,
    budget: &Budget<'_>,
) -> Result<Option<Range<usize>>, Error> {
    if packet.len() > limits.max_xmp_bytes {
        return Err(Error::LimitExceeded(Limit::XmpBytes));
    }
    budget.check_cancel()?;
    let mut found = None;
    scan(packet, limits, budget, &mut |ns, name, span, top| {
        if top && ns == NS_TIFF.as_bytes() && name == b"Orientation" {
            found = Some(span);
            Ok(ControlFlow::Break(()))
        } else {
            Ok(ControlFlow::Continue(()))
        }
    })?;
    Ok(found)
}

const NS_CAMERA: &[u8] = b"http://ns.google.com/photos/1.0/camera/";
const NS_CONTAINER: &[u8] = b"http://ns.google.com/photos/1.0/container/";
const NS_ITEM: &[u8] = b"http://ns.google.com/photos/1.0/container/item/";
/// The Motion Photo and MicroVideo fields a container description keeps, in output order.
const CAMERA_FIELDS: [&str; 7] = [
    "MotionPhoto",
    "MotionPhotoVersion",
    "MotionPhotoPresentationTimestampUs",
    "MicroVideo",
    "MicroVideoVersion",
    "MicroVideoOffset",
    "MicroVideoPresentationTimestampUs",
];
/// The `Container:Item` fields, in output order.
const ITEM_FIELDS: [&str; 6] = ["Mime", "Semantic", "Length", "Padding", "Label", "URI"];
/// The index of `Item:Length` in [`Item`].
pub(crate) const ITEM_LENGTH: usize = 2;
/// The index of `Item:Padding` in [`Item`].
pub(crate) const ITEM_PADDING: usize = 3;

/// The raw value ranges of the fields of one `Container:Item`: Mime, Semantic, Length, Padding,
/// Label, URI.
pub(crate) type Item = [Option<Range<usize>>; 6];

/// What an XMP packet says about the data after a JPEG's primary image, as raw value ranges in
/// the packet: the top-level `hdrgm:Version`, Motion Photo and MicroVideo fields (the first of
/// each) and the items of `Container:Directory` in document order. The capacity of `items` is
/// charged to the budget until it is dropped.
pub(crate) struct ContainerDesc<'a, 'b> {
    pub(crate) version: Option<Range<usize>>,
    camera: [Option<Range<usize>>; 7],
    pub(crate) items: Vec<Item>,
    charged: usize,
    budget: &'a Budget<'b>,
}

impl ContainerDesc<'_, '_> {
    pub(crate) fn is_empty(&self) -> bool {
        self.version.is_none() && self.camera.iter().all(Option::is_none) && self.items.is_empty()
    }

    fn ranges(&self) -> impl Iterator<Item = &Range<usize>> {
        self.version
            .iter()
            .chain(self.camera.iter().flatten())
            .chain(self.items.iter().flatten().flatten())
    }
}

impl Drop for ContainerDesc<'_, '_> {
    fn drop(&mut self) {
        self.budget.release(self.charged);
    }
}

/// The container description of `packet` (a standard XMP packet). Items are `Container:Item`
/// elements (namespace `http://ns.google.com/photos/1.0/container/`, under any prefix); their
/// fields (namespace `http://ns.google.com/photos/1.0/container/item/`) are attributes of the
/// item or simple properties inside it. The scan is charged to `budget` and checks its cancel
/// flag every 1024 nodes.
///
/// # Errors
///
/// As [`Xmp::parse`]: `LimitExceeded` for `max_xmp_bytes`, `max_xmp_nodes` and `max_alloc`,
/// `OutOfMemory`, `Cancelled`.
pub(crate) fn container_desc<'a, 'b>(
    packet: &[u8],
    limits: &Limits,
    budget: &'a Budget<'b>,
) -> Result<ContainerDesc<'a, 'b>, Error> {
    if packet.len() > limits.max_xmp_bytes {
        return Err(Error::LimitExceeded(Limit::XmpBytes));
    }
    budget.check_cancel()?;
    let mut d = ContainerDesc {
        version: None,
        camera: Default::default(),
        items: Vec::new(),
        charged: 0,
        budget,
    };
    // The attributes reported next belong to a `Container:Item` start tag.
    let mut in_item = false;
    scan_events(packet, limits, budget, true, &mut |ev| {
        match ev {
            Ev::Start(ns, name) => {
                in_item = ns == NS_CONTAINER && name == b"Item";
                if in_item {
                    reserve_one(&mut d.items, budget, &mut d.charged)?;
                    d.items.push(Item::default());
                }
            }
            Ev::Attr(ns, name, r) => {
                if in_item && ns == NS_ITEM {
                    set_item(&mut d.items, name, r);
                }
            }
            Ev::Prop(ns, name, r, top) => {
                in_item = false;
                if top && ns == NS_HDRGM.as_bytes() && name == b"Version" {
                    if d.version.is_none() {
                        d.version = Some(r);
                    }
                } else if top && ns == NS_CAMERA {
                    if let Some(i) = CAMERA_FIELDS.iter().position(|f| f.as_bytes() == name)
                        && let Some(slot) = d.camera.get_mut(i)
                        && slot.is_none()
                    {
                        *slot = Some(r);
                    }
                } else if !top && ns == NS_ITEM {
                    set_item(&mut d.items, name, r);
                }
            }
        }
        Ok(ControlFlow::Continue(()))
    })?;
    Ok(d)
}

/// Sets the field `name` of the last item, unless it is set already.
fn set_item(items: &mut [Item], name: &[u8], r: Range<usize>) {
    if let Some(item) = items.last_mut()
        && let Some(i) = ITEM_FIELDS.iter().position(|f| f.as_bytes() == name)
        && let Some(slot) = item.get_mut(i)
        && slot.is_none()
    {
        *slot = Some(r);
    }
}

/// The raw text at `r` of `packet` as a number, surrounding whitespace ignored.
pub(crate) fn number(packet: &[u8], r: &Range<usize>) -> Option<usize> {
    std::str::from_utf8(packet.get(r.clone())?)
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// A new XMP packet with the fields of `d` only: `x:xmpmeta`, one `rdf:Description` with
/// `hdrgm:Version` and the Camera fields as attributes, then `Container:Directory` as an
/// `rdf:Seq` of `rdf:li rdf:parseType="Resource"`, each with one `Container:Item` whose fields
/// are attributes. The prefixes are always hdrgm, Camera, Container and Item. Values are decoded
/// from `packet` (entities, CDATA) and escaped again. `None` when `d` is empty or the packet
/// would be longer than `max` bytes. The packet is charged to the budget of `d`.
///
/// # Errors
///
/// `LimitExceeded(Alloc)`, `OutOfMemory`.
pub(crate) fn container_packet(
    packet: &[u8],
    d: &ContainerDesc,
    max: usize,
) -> Result<Option<Vec<u8>>, Error> {
    if d.is_empty() {
        return Ok(None);
    }
    // Decoded text is never longer than the raw text, so this buffer does not grow.
    let longest = d.ranges().map(|r| r.len()).max().unwrap_or(0);
    let mut scratch = d.budget.vec::<u8>(longest)?;
    let mut n = 0usize;
    emit(packet, d, &mut scratch, &mut |b: &[u8]| {
        n = n.saturating_add(b.len())
    });
    let out = if n > max {
        None
    } else {
        let mut out = d.budget.vec::<u8>(n)?;
        emit(packet, d, &mut scratch, &mut |b: &[u8]| {
            out.extend_from_slice(b)
        });
        Some(out)
    };
    d.budget.release(longest);
    Ok(out)
}

fn emit(packet: &[u8], d: &ContainerDesc, scratch: &mut Vec<u8>, w: &mut dyn FnMut(&[u8])) {
    w(b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
        xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" \
        xmlns:hdrgm=\"http://ns.adobe.com/hdr-gain-map/1.0/\" \
        xmlns:Camera=\"http://ns.google.com/photos/1.0/camera/\" \
        xmlns:Container=\"http://ns.google.com/photos/1.0/container/\" \
        xmlns:Item=\"http://ns.google.com/photos/1.0/container/item/\"");
    let raw = |r: &Range<usize>| packet.get(r.clone()).unwrap_or_default();
    if let Some(r) = &d.version {
        attr(w, scratch, raw(r), "hdrgm", "Version");
    }
    for (r, name) in d.camera.iter().zip(CAMERA_FIELDS) {
        if let Some(r) = r {
            attr(w, scratch, raw(r), "Camera", name);
        }
    }
    if d.items.is_empty() {
        w(b"/>");
    } else {
        w(b"><Container:Directory><rdf:Seq>");
        for item in &d.items {
            w(b"<rdf:li rdf:parseType=\"Resource\"><Container:Item");
            for (r, name) in item.iter().zip(ITEM_FIELDS) {
                if let Some(r) = r {
                    attr(w, scratch, raw(r), "Item", name);
                }
            }
            w(b"/></rdf:li>");
        }
        w(b"</rdf:Seq></Container:Directory></rdf:Description>");
    }
    w(b"</rdf:RDF></x:xmpmeta>");
}

/// ` prefix:name="value"`, the value decoded and escaped for a double-quoted attribute.
fn attr(w: &mut dyn FnMut(&[u8]), scratch: &mut Vec<u8>, raw: &[u8], prefix: &str, name: &str) {
    w(b" ");
    w(prefix.as_bytes());
    w(b":");
    w(name.as_bytes());
    w(b"=\"");
    scratch.clear();
    decode_text(raw, scratch);
    for c in scratch.iter() {
        let esc: &[u8] = match c {
            b'&' => &b"&amp;"[..],
            b'<' => &b"&lt;"[..],
            b'>' => &b"&gt;"[..],
            b'"' => &b"&quot;"[..],
            _ => std::slice::from_ref(c),
        };
        w(esc);
    }
    w(b"\"");
}

fn collect(
    data: &[u8],
    limits: &Limits,
    budget: &Budget<'_>,
    out: &mut Vec<Property>,
) -> Result<(), Error> {
    scan(data, limits, budget, &mut |ns, name, span, top_level| {
        let raw = data.get(span).unwrap_or_default();
        if let (Ok(ns), Ok(name)) = (std::str::from_utf8(ns), std::str::from_utf8(name)) {
            let mut bytes = budget.vec::<u8>(raw.len())?;
            decode_text(raw, &mut bytes);
            match String::from_utf8(bytes) {
                Ok(value) => out.push(Property {
                    ns: charged_str(ns, budget)?,
                    name: charged_str(name, budget)?,
                    value,
                    top_level,
                }),
                // The decode buffer was charged at the raw length.
                Err(_) => budget.release(raw.len()),
            }
        }
        Ok(ControlFlow::Continue(()))
    })
}

fn charged_str(s: &str, budget: &Budget<'_>) -> Result<String, Error> {
    budget.charge(s.len())?;
    let mut out = String::new();
    if out.try_reserve_exact(s.len()).is_err() {
        budget.release(s.len());
        return Err(Error::OutOfMemory);
    }
    out.push_str(s);
    Ok(out)
}

/// Resolves entities and CDATA and removes comments. The output is never longer than `raw`, so
/// the buffer reserved for it does not grow.
fn decode_text(raw: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while let Some(&b) = raw.get(i) {
        let rest = at(raw, i);
        if rest.starts_with(b"<!--") {
            match find_seq(raw, i + 4, b"-->") {
                Some(e) => i = e + 3,
                None => break,
            }
        } else if rest.starts_with(b"<![CDATA[") {
            let end = find_seq(raw, i + 9, b"]]>");
            out.extend_from_slice(raw.get(i + 9..end.unwrap_or(raw.len())).unwrap_or_default());
            match end {
                Some(e) => i = e + 3,
                None => break,
            }
        } else if b == b'&'
            && let Some((ch, len)) = entity(rest)
        {
            out.extend_from_slice(ch.encode_utf8(&mut [0u8; 4]).as_bytes());
            i += len;
        } else {
            out.push(b);
            i += 1;
        }
    }
}

/// `&name;` or `&#N;` / `&#xH;` at the start of `s`: the character and the bytes it spans.
fn entity(s: &[u8]) -> Option<(char, usize)> {
    let end = s
        .get(..12usize.min(s.len()))?
        .iter()
        .position(|&c| c == b';')?;
    let name = s.get(1..end)?;
    let ch = match name {
        b"lt" => '<',
        b"gt" => '>',
        b"amp" => '&',
        b"quot" => '"',
        b"apos" => '\'',
        _ => {
            let num = name.strip_prefix(b"#")?;
            let v = match num.strip_prefix(b"x") {
                Some(h) => u32::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()?,
                None => std::str::from_utf8(num).ok()?.parse().ok()?,
            };
            char::from_u32(v)?
        }
    };
    Some((ch, end + 1))
}

fn at(b: &[u8], off: usize) -> &[u8] {
    b.get(off..).unwrap_or_default()
}

fn find(data: &[u8], from: usize, b: u8) -> Option<usize> {
    at(data, from)
        .iter()
        .position(|&c| c == b)
        .map(|i| i + from)
}

fn find_seq(data: &[u8], from: usize, pat: &[u8]) -> Option<usize> {
    at(data, from)
        .windows(pat.len())
        .position(|w| w == pat)
        .map(|i| i + from)
}

fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

/// The index after an optional UTF-8 BOM and whitespace.
fn body_start(data: &[u8]) -> usize {
    let mut i = if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
        3
    } else {
        0
    };
    while data.get(i).copied().is_some_and(is_ws) {
        i += 1;
    }
    i
}

/// Attributes of a start tag as (name range, value range inside the quotes).
struct Attrs<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
}

impl Iterator for Attrs<'_> {
    type Item = (Range<usize>, Range<usize>);

    fn next(&mut self) -> Option<Self::Item> {
        let d = self.data;
        loop {
            while self.pos < self.end && d.get(self.pos).copied().is_some_and(is_ws) {
                self.pos += 1;
            }
            if self.pos >= self.end {
                return None;
            }
            let ns = self.pos;
            while self.pos < self.end && d.get(self.pos).is_some_and(|&c| !is_ws(c) && c != b'=') {
                self.pos += 1;
            }
            let ne = self.pos;
            while self.pos < self.end && d.get(self.pos).copied().is_some_and(is_ws) {
                self.pos += 1;
            }
            if d.get(self.pos) != Some(&b'=') {
                continue;
            }
            self.pos += 1;
            while self.pos < self.end && d.get(self.pos).copied().is_some_and(is_ws) {
                self.pos += 1;
            }
            let Some(&q) = d.get(self.pos).filter(|&&c| c == b'"' || c == b'\'') else {
                continue;
            };
            self.pos += 1;
            let vs = self.pos;
            let off = d.get(vs..self.end)?.iter().position(|&c| c == q)?;
            self.pos = vs + off + 1;
            return Some((ns..ne, vs..vs + off));
        }
    }
}

/// The end of a start tag (its `>`), skipping quoted attribute values.
fn tag_end(data: &[u8], from: usize) -> Option<usize> {
    let mut quote = 0u8;
    for (i, &b) in at(data, from).iter().enumerate() {
        if quote != 0 {
            if b == quote {
                quote = 0;
            }
        } else if b == b'"' || b == b'\'' {
            quote = b;
        } else if b == b'>' {
            return Some(from + i);
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Other,
    /// `rdf:Description`: its attributes are properties.
    Desc,
    /// `rdf:Seq`, `rdf:Bag`, `rdf:Alt`.
    Container,
    /// `rdf:li` inside a container.
    Li,
    /// A property element (child of a description, structure or list item).
    Prop,
}

type NameRanges = (Range<usize>, Range<usize>);

struct Frame {
    name: Range<usize>,
    ns_mark: usize,
    role: Role,
    has_child: bool,
    text_start: usize,
    /// Namespace URI and local name of the property this element (or its list) stands for.
    prop: Option<NameRanges>,
    /// Inside a property element, array or list item (a description here holds struct fields).
    nested: bool,
    /// A top-level description, a property of one, or that property's array or list item.
    top: bool,
}

/// Makes room for one more item in `v`. The capacity a growth adds is charged and stays charged
/// until the scan ends (a `Vec` keeps its capacity when items are removed).
fn reserve_one<T>(v: &mut Vec<T>, budget: &Budget<'_>, charged: &mut usize) -> Result<(), Error> {
    if v.len() < v.capacity() {
        return Ok(());
    }
    let add = v.capacity().max(4);
    let bytes = add
        .checked_mul(size_of::<T>())
        .ok_or(Error::LimitExceeded(Limit::Alloc))?;
    budget.charge(bytes)?;
    if v.try_reserve_exact(add).is_err() {
        budget.release(bytes);
        return Err(Error::OutOfMemory);
    }
    *charged = charged.saturating_add(bytes);
    Ok(())
}

/// The element and namespace stacks; their capacity is charged to the budget.
struct Stacks {
    frames: Vec<Frame>,
    /// (prefix range, URI range) per `xmlns` declaration in scope.
    nss: Vec<NameRanges>,
    charged: usize,
}

impl Stacks {
    fn push_ns(
        &mut self,
        budget: &Budget<'_>,
        prefix: Range<usize>,
        uri: Range<usize>,
    ) -> Result<(), Error> {
        reserve_one(&mut self.nss, budget, &mut self.charged)?;
        self.nss.push((prefix, uri));
        Ok(())
    }

    fn push_frame(&mut self, budget: &Budget<'_>, f: Frame) -> Result<(), Error> {
        reserve_one(&mut self.frames, budget, &mut self.charged)?;
        self.frames.push(f);
        Ok(())
    }

    fn pop_frame(&mut self) -> Option<Frame> {
        self.frames.pop()
    }

    fn truncate_ns(&mut self, mark: usize) {
        self.nss.truncate(mark);
    }

    fn resolve(&self, data: &[u8], prefix: &[u8]) -> Option<Range<usize>> {
        self.nss
            .iter()
            .rev()
            .find(|(p, _)| data.get(p.clone()) == Some(prefix))
            .map(|(_, u)| u.clone())
    }
}

type Sink<'a> =
    &'a mut dyn FnMut(&[u8], &[u8], Range<usize>, bool) -> Result<ControlFlow<()>, Error>;

/// What [`scan_events`] reports, in document order.
enum Ev<'a> {
    /// A simple property: namespace URI, local name, raw value range, top-level.
    Prop(&'a [u8], &'a [u8], Range<usize>, bool),
    /// The start tag of a property element: namespace URI, local name.
    Start(&'a [u8], &'a [u8]),
    /// An attribute of the start tag reported just before, other than `xmlns`, `xml:*` and
    /// `rdf:*`: namespace URI, local name, raw value range.
    Attr(&'a [u8], &'a [u8], Range<usize>),
}

type EvSink<'a> = &'a mut dyn FnMut(Ev<'_>) -> Result<ControlFlow<()>, Error>;

/// Walks the XML and calls `f(namespace URI, local name, raw value range, top-level)` for each
/// simple property; `Break` from `f` stops the scan.
fn scan(data: &[u8], limits: &Limits, budget: &Budget<'_>, f: Sink<'_>) -> Result<(), Error> {
    scan_events(data, limits, budget, false, &mut |ev| match ev {
        Ev::Prop(ns, name, span, top) => f(ns, name, span, top),
        Ev::Start(..) | Ev::Attr(..) => Ok(ControlFlow::Continue(())),
    })
}

/// [`scan`], reporting [`Ev`]s; the start tags of property elements and their attributes only
/// with `tags`.
fn scan_events(
    data: &[u8],
    limits: &Limits,
    budget: &Budget<'_>,
    tags: bool,
    f: EvSink<'_>,
) -> Result<(), Error> {
    let mut st = Stacks {
        frames: Vec::new(),
        nss: Vec::new(),
        charged: 0,
    };
    let r = scan_inner(data, limits, budget, &mut st, tags, f);
    budget.release(st.charged);
    r
}

fn tick(nodes: &mut usize, limits: &Limits, budget: &Budget<'_>) -> Result<(), Error> {
    *nodes += 1;
    if *nodes > limits.max_xmp_nodes {
        Err(Error::LimitExceeded(Limit::XmpNodes))
    } else if nodes.is_multiple_of(1024) {
        budget.check_cancel()
    } else {
        Ok(())
    }
}

fn scan_inner(
    data: &[u8],
    limits: &Limits,
    budget: &Budget<'_>,
    st: &mut Stacks,
    tags: bool,
    f: EvSink<'_>,
) -> Result<(), Error> {
    let mut pos = body_start(data);
    let mut nodes = 0usize;
    while let Some(lt) = find(data, pos, b'<') {
        let rest = at(data, lt);
        // Comments, CDATA, processing instructions and declarations outside values.
        let skip = if rest.starts_with(b"<!--") {
            find_seq(data, lt + 4, b"-->").map(|e| e + 3)
        } else if rest.starts_with(b"<![CDATA[") {
            find_seq(data, lt + 9, b"]]>").map(|e| e + 3)
        } else if rest.starts_with(b"<?") {
            find_seq(data, lt + 2, b"?>").map(|e| e + 2)
        } else if rest.starts_with(b"<!") {
            find(data, lt + 2, b'>').map(|e| e + 1)
        } else {
            Some(0)
        };
        match skip {
            None => break,
            Some(0) => {}
            Some(next) => {
                pos = next;
                continue;
            }
        }

        if rest.starts_with(b"</") {
            let Some(gt) = find(data, lt + 2, b'>') else {
                break;
            };
            pos = gt + 1;
            let name = data.get(lt + 2..gt).unwrap_or_default().trim_ascii();
            let from = st.frames.len().saturating_sub(CLOSE_LOOKBACK);
            let idx = st
                .frames
                .iter()
                .enumerate()
                .skip(from)
                .rev()
                .find(|(_, fr)| data.get(fr.name.clone()) == Some(name))
                .map(|(i, _)| i);
            let Some(idx) = idx else { continue };
            while st.frames.len() > idx + 1 {
                st.pop_frame();
            }
            let Some(fr) = st.pop_frame() else {
                continue;
            };
            st.truncate_ns(fr.ns_mark);
            if matches!(fr.role, Role::Prop | Role::Li)
                && !fr.has_child
                && let Some((u, l)) = &fr.prop
            {
                let ub = data.get(u.clone()).unwrap_or_default();
                let lb = data.get(l.clone()).unwrap_or_default();
                if f(Ev::Prop(ub, lb, fr.text_start..lt, fr.top))?.is_break() {
                    return Ok(());
                }
            }
            continue;
        }

        // A start tag.
        let name_start = lt + 1;
        let Some(rel) = at(data, name_start)
            .iter()
            .position(|&c| is_ws(c) || c == b'/' || c == b'>')
        else {
            break;
        };
        let name_end = name_start + rel;
        if rel == 0 {
            pos = lt + 1;
            continue;
        }
        let Some(gt) = tag_end(data, name_end) else {
            break;
        };
        let self_closing = gt > name_end && data.get(gt - 1) == Some(&b'/');
        let attrs_end = if self_closing { gt - 1 } else { gt };
        let attrs = || Attrs {
            data,
            pos: name_end,
            end: attrs_end,
        };
        pos = gt + 1;

        tick(&mut nodes, limits, budget)?;
        if let Some(p) = st.frames.last_mut() {
            p.has_child = true;
        }
        let mark = st.nss.len();
        for (an, av) in attrs() {
            tick(&mut nodes, limits, budget)?;
            let aname = data.get(an.clone()).unwrap_or_default();
            let prefix = if aname == b"xmlns" {
                Some(an.start..an.start)
            } else if aname.starts_with(b"xmlns:") {
                Some(an.start + 6..an.end)
            } else {
                None
            };
            if let Some(prefix) = prefix {
                if st.nss.len() >= MAX_NS {
                    return Ok(());
                }
                st.push_ns(budget, prefix, av)?;
            }
        }

        let qname = data.get(name_start..name_end).unwrap_or_default();
        let (prefix, local_r) = match qname.iter().position(|&c| c == b':') {
            Some(i) => (
                qname.get(..i).unwrap_or_default(),
                name_start + i + 1..name_end,
            ),
            None => (qname.get(..0).unwrap_or_default(), name_start..name_end),
        };
        let uri = st.resolve(data, prefix);
        let uri_b = uri.as_ref().and_then(|u| data.get(u.clone()));
        let local = data.get(local_r.clone()).unwrap_or_default();
        let (parent_role, parent_prop, parent_nested, parent_top) =
            st.frames.last().map_or((None, None, false, false), |p| {
                (Some(p.role), p.prop.clone(), p.nested, p.top)
            });
        let nested =
            parent_nested || matches!(parent_role, Some(Role::Prop | Role::Container | Role::Li));
        let (role, prop) = if uri_b == Some(NS_RDF) {
            match local {
                b"Description" => (Role::Desc, None),
                b"Seq" | b"Bag" | b"Alt" if parent_role == Some(Role::Prop) => {
                    (Role::Container, parent_prop)
                }
                b"li" if parent_role == Some(Role::Container) => (Role::Li, parent_prop),
                _ => (Role::Other, None),
            }
        } else if matches!(parent_role, Some(Role::Desc | Role::Prop | Role::Li)) {
            (Role::Prop, uri.map(|u| (u, local_r)))
        } else {
            (Role::Other, None)
        };
        let top = match role {
            Role::Desc => !nested,
            Role::Prop => parent_role == Some(Role::Desc) && parent_top,
            Role::Container | Role::Li => parent_top,
            Role::Other => false,
        };

        let prop_tag = tags && role == Role::Prop;
        if prop_tag
            && let Some(u) = uri_b
            && f(Ev::Start(u, local))?.is_break()
        {
            return Ok(());
        }
        if role == Role::Desc || prop_tag {
            for (an, av) in attrs() {
                let aname = data.get(an).unwrap_or_default();
                let Some(c) = aname.iter().position(|&c| c == b':') else {
                    continue;
                };
                let ap = aname.get(..c).unwrap_or_default();
                if ap == b"xmlns" || ap == b"xml" {
                    continue;
                }
                let Some(u) = st.resolve(data, ap) else {
                    continue;
                };
                let ub = data.get(u).unwrap_or_default();
                if ub == NS_RDF {
                    continue;
                }
                let name = at(aname, c + 1);
                let ev = if role == Role::Desc {
                    Ev::Prop(ub, name, av, top)
                } else {
                    Ev::Attr(ub, name, av)
                };
                if f(ev)?.is_break() {
                    return Ok(());
                }
            }
        }

        if self_closing {
            st.truncate_ns(mark);
        } else {
            st.push_frame(
                budget,
                Frame {
                    name: name_start..name_end,
                    ns_mark: mark,
                    role,
                    has_child: false,
                    text_start: pos,
                    prop,
                    nested,
                    top,
                },
            )?;
        }
    }
    Ok(())
}
