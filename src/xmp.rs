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
            scan(data, limits, budget, &mut |_, _, _| {
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

    /// The text of the first property with that namespace URI and local name.
    pub fn get(&self, ns: &str, name: &str) -> Option<&str> {
        self.props
            .iter()
            .find(|p| p.ns == ns && p.name == name)
            .map(|p| p.value.as_str())
    }

    /// `tiff:Orientation` (`http://ns.adobe.com/tiff/1.0/`) as a number; surrounding whitespace is
    /// ignored. `None` when absent or not a `u16`. ExifTool prints the integer.
    pub fn orientation(&self) -> Option<u16> {
        self.get(NS_TIFF, "Orientation")?.trim().parse().ok()
    }

    /// `xmp:CreateDate` (`http://ns.adobe.com/xap/1.0/`), the raw text as stored (for example
    /// `2020-01-02T03:04:05Z`; ExifTool rewrites the date to `2020:01:02 03:04:05Z`).
    pub fn create_date(&self) -> Option<&str> {
        self.get(NS_XMP, "CreateDate")
    }

    /// `xmp:Rating`, the raw text as stored (ExifTool prints it as a number).
    pub fn rating(&self) -> Option<&str> {
        self.get(NS_XMP, "Rating")
    }

    /// The properties of the HDR gain map namespace `http://ns.adobe.com/hdr-gain-map/1.0/`
    /// (`hdrgm:Version`, `hdrgm:GainMapMax`, ...), raw text.
    pub fn hdrgm(&self) -> impl Iterator<Item = &Property> {
        self.props.iter().filter(|p| p.ns == NS_HDRGM)
    }
}

/// The byte range in `packet` of the raw text of the first `tiff:Orientation` value (the
/// attribute value without quotes, or the element text). `None` when absent or when a limit
/// stops the scan.
pub(crate) fn orientation_value_span(packet: &[u8], limits: &Limits) -> Option<Range<usize>> {
    let cancel = AtomicBool::new(false);
    let budget = Budget::new(limits, &cancel);
    let mut found = None;
    scan(packet, limits, &budget, &mut |ns, name, span| {
        if ns == NS_TIFF.as_bytes() && name == b"Orientation" {
            found = Some(span);
            Ok(ControlFlow::Break(()))
        } else {
            Ok(ControlFlow::Continue(()))
        }
    })
    .ok()?;
    found
}

fn collect(
    data: &[u8],
    limits: &Limits,
    budget: &Budget<'_>,
    out: &mut Vec<Property>,
) -> Result<(), Error> {
    scan(data, limits, budget, &mut |ns, name, span| {
        let raw = data.get(span).unwrap_or_default();
        if let (Ok(ns), Ok(name)) = (std::str::from_utf8(ns), std::str::from_utf8(name)) {
            let mut bytes = budget.vec::<u8>(raw.len())?;
            decode_text(raw, &mut bytes);
            match String::from_utf8(bytes) {
                Ok(value) => out.push(Property {
                    ns: charged_str(ns, budget)?,
                    name: charged_str(name, budget)?,
                    value,
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
}

// Twice the size: a push may grow the vector's capacity.
const FRAME_COST: usize = 2 * size_of::<Frame>();
const NS_COST: usize = 2 * size_of::<NameRanges>();

/// The element and namespace stacks; their memory is charged to the budget.
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
        budget.charge(NS_COST)?;
        self.charged += NS_COST;
        self.nss.push((prefix, uri));
        Ok(())
    }

    fn push_frame(&mut self, budget: &Budget<'_>, f: Frame) -> Result<(), Error> {
        budget.charge(FRAME_COST)?;
        self.charged += FRAME_COST;
        self.frames.push(f);
        Ok(())
    }

    fn pop_frame(&mut self, budget: &Budget<'_>) -> Option<Frame> {
        let f = self.frames.pop()?;
        budget.release(FRAME_COST);
        self.charged = self.charged.saturating_sub(FRAME_COST);
        Some(f)
    }

    fn truncate_ns(&mut self, budget: &Budget<'_>, mark: usize) {
        let removed = self.nss.len().saturating_sub(mark) * NS_COST;
        self.nss.truncate(mark);
        budget.release(removed);
        self.charged = self.charged.saturating_sub(removed);
    }

    fn resolve(&self, data: &[u8], prefix: &[u8]) -> Option<Range<usize>> {
        self.nss
            .iter()
            .rev()
            .find(|(p, _)| data.get(p.clone()) == Some(prefix))
            .map(|(_, u)| u.clone())
    }
}

type Sink<'a> = &'a mut dyn FnMut(&[u8], &[u8], Range<usize>) -> Result<ControlFlow<()>, Error>;

/// Walks the XML and calls `f(namespace URI, local name, raw value range)` for each simple
/// property; `Break` from `f` stops the scan.
fn scan(data: &[u8], limits: &Limits, budget: &Budget<'_>, f: Sink<'_>) -> Result<(), Error> {
    let mut st = Stacks {
        frames: Vec::new(),
        nss: Vec::new(),
        charged: 0,
    };
    let r = scan_inner(data, limits, budget, &mut st, f);
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
    f: Sink<'_>,
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
                st.pop_frame(budget);
            }
            let Some(fr) = st.pop_frame(budget) else {
                continue;
            };
            st.truncate_ns(budget, fr.ns_mark);
            if matches!(fr.role, Role::Prop | Role::Li)
                && !fr.has_child
                && let Some((u, l)) = &fr.prop
            {
                let ub = data.get(u.clone()).unwrap_or_default();
                let lb = data.get(l.clone()).unwrap_or_default();
                if f(ub, lb, fr.text_start..lt)?.is_break() {
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
        let (parent_role, parent_prop) = st
            .frames
            .last()
            .map_or((None, None), |p| (Some(p.role), p.prop.clone()));
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

        if role == Role::Desc {
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
                if ub != NS_RDF && f(ub, at(aname, c + 1), av)?.is_break() {
                    return Ok(());
                }
            }
        }

        if self_closing {
            st.truncate_ns(budget, mark);
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
                },
            )?;
        }
    }
    Ok(())
}
