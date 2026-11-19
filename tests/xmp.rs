//! Integration tests for `Xmp::parse`: packets are built in code, no binary fixtures.

use std::sync::atomic::AtomicBool;

use exif::{Error, Limit, Limits, Xmp};

const TIFF: &str = "http://ns.adobe.com/tiff/1.0/";
const XMP: &str = "http://ns.adobe.com/xap/1.0/";
const DC: &str = "http://purl.org/dc/elements/1.1/";
const HDR: &str = "http://ns.adobe.com/hdr-gain-map/1.0/";
const EXIF: &str = "http://ns.adobe.com/exif/1.0/";

const HEAD: &str = "<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"test\">\n<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n";
const TAIL: &str = "</rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>";
const NSDECL: &str = "xmlns:tiff=\"http://ns.adobe.com/tiff/1.0/\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:hdrgm=\"http://ns.adobe.com/hdr-gain-map/1.0/\" xmlns:exif=\"http://ns.adobe.com/exif/1.0/\"";

/// A packet with one `rdf:Description` that has the given attributes and body.
fn packet(attrs: &str, body: &str) -> Vec<u8> {
    format!(
        "{HEAD}<rdf:Description rdf:about=\"\" {NSDECL} {attrs}>{body}</rdf:Description>\n{TAIL}"
    )
    .into_bytes()
}

fn lim() -> Limits {
    Limits {
        max_threads: 1,
        ..Limits::default()
    }
}

fn parse(p: &[u8]) -> Result<Xmp, Error> {
    Xmp::parse(p, None, &lim(), &AtomicBool::new(false))
}

fn props(x: &Xmp) -> Vec<(&str, &str, &str)> {
    x.properties()
        .iter()
        .map(|p| (p.ns.as_str(), p.name.as_str(), p.value.as_str()))
        .collect()
}

#[test]
fn attributes_form() {
    let p = packet(
        "tiff:Orientation=\"6\" xmp:CreateDate='2024-05-06T07:08:09Z' xmp:Rating=\"4\"",
        "",
    );
    let x = parse(&p).unwrap();
    assert_eq!(x.raw(), &p[..]);
    assert_eq!(
        props(&x),
        [
            (TIFF, "Orientation", "6"),
            (XMP, "CreateDate", "2024-05-06T07:08:09Z"),
            (XMP, "Rating", "4"),
        ]
    );
    assert_eq!(x.orientation(), Some(6));
    assert_eq!(x.create_date(), Some("2024-05-06T07:08:09Z"));
    assert_eq!(x.rating(), Some("4"));
    assert_eq!(x.get(TIFF, "Orientation"), Some("6"));
    assert_eq!(x.get(XMP, "Orientation"), None);
    // rdf:about, xmlns:* and rdf:/xml: attributes are not properties.
    assert!(x.properties().iter().all(|p| p.name != "about"));
}

#[test]
fn element_form_keeps_text_as_stored() {
    let p = packet(
        "",
        "<tiff:Orientation>8</tiff:Orientation>\n<xmp:Rating>\n  3\n</xmp:Rating>",
    );
    let x = parse(&p).unwrap();
    assert_eq!(x.orientation(), Some(8));
    assert_eq!(x.rating(), Some("\n  3\n"));
    let p = packet("", "<tiff:Orientation>\n 6 \n</tiff:Orientation>");
    assert_eq!(parse(&p).unwrap().orientation(), Some(6));
    let p = packet("tiff:Orientation=\"six\"", "");
    assert_eq!(parse(&p).unwrap().orientation(), None);
}

#[test]
fn first_property_wins() {
    let p = packet(
        "tiff:Orientation=\"3\"",
        "<tiff:Orientation>5</tiff:Orientation>",
    );
    let x = parse(&p).unwrap();
    assert_eq!(x.orientation(), Some(3));
    assert_eq!(x.properties().len(), 2);
}

#[test]
fn seq_bag_alt_arrays() {
    let p = packet(
        "",
        "<dc:subject><rdf:Bag><rdf:li>a</rdf:li><rdf:li>b</rdf:li></rdf:Bag></dc:subject>\
         <dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Title</rdf:li></rdf:Alt></dc:title>\
         <dc:creator><rdf:Seq><rdf:li>Ann</rdf:li><rdf:li>Bob</rdf:li></rdf:Seq></dc:creator>",
    );
    let x = parse(&p).unwrap();
    assert_eq!(
        props(&x),
        [
            (DC, "subject", "a"),
            (DC, "subject", "b"),
            (DC, "title", "Title"),
            (DC, "creator", "Ann"),
            (DC, "creator", "Bob"),
        ]
    );
}

#[test]
fn structure_fields_are_properties() {
    let p = packet(
        "",
        "<exif:Flash rdf:parseType=\"Resource\"><exif:Fired>True</exif:Fired>\
         <exif:Mode>1</exif:Mode></exif:Flash>",
    );
    let x = parse(&p).unwrap();
    assert_eq!(props(&x), [(EXIF, "Fired", "True"), (EXIF, "Mode", "1")]);
}

#[test]
fn entities() {
    let p = packet(
        "",
        "<dc:description>&lt;a&gt; &amp; &quot;b&quot; &apos;c&apos; &#65;&#x42;&#x1F600; &foo; a & b &#xZZ; &#;</dc:description>",
    );
    let x = parse(&p).unwrap();
    assert_eq!(
        x.get(DC, "description"),
        Some("<a> & \"b\" 'c' AB\u{1F600} &foo; a & b &#xZZ; &#;")
    );
    let p = packet("dc:format=\"a&amp;b&#10;c\"", "");
    assert_eq!(parse(&p).unwrap().get(DC, "format"), Some("a&b\nc"));
}

#[test]
fn cdata() {
    let p = packet(
        "",
        "<dc:description>x<![CDATA[<b>&amp;</b>]]>y&amp;</dc:description>\
         <dc:source><![CDATA[only]]></dc:source>",
    );
    let x = parse(&p).unwrap();
    assert_eq!(x.get(DC, "description"), Some("x<b>&amp;</b>y&"));
    assert_eq!(x.get(DC, "source"), Some("only"));
}

#[test]
fn comments() {
    let p = packet(
        "",
        "<!-- <xmp:Rating>9</xmp:Rating> -->\n<dc:format>a<!-- inner -->b</dc:format>\
         <!--x--><tiff:Orientation>3</tiff:Orientation>",
    );
    let x = parse(&p).unwrap();
    assert_eq!(x.rating(), None);
    assert_eq!(x.get(DC, "format"), Some("ab"));
    assert_eq!(x.orientation(), Some(3));
    // An unterminated comment ends the scan; what came before stays.
    let p = packet("tiff:Orientation=\"2\"", "<!-- never closed");
    assert_eq!(parse(&p).unwrap().orientation(), Some(2));
}

#[test]
fn other_prefixes_and_scopes() {
    let doc = format!(
        "{HEAD}<rdf:Description xmlns:t=\"{TIFF}\" xmlns:p=\"urn:a\" t:Orientation=\"3\" p:x=\"1\"/>\
         <rdf:Description xmlns:p=\"urn:b\" p:x=\"2\" q:y=\"lost\"/>\
         <rdf:Description xmlns=\"urn:d\" xmlns:p=\"urn:a\"><p:z>3</p:z><k>4</k></rdf:Description>\
         {TAIL}"
    );
    let x = parse(doc.as_bytes()).unwrap();
    assert_eq!(x.orientation(), Some(3));
    assert_eq!(
        props(&x),
        [
            (TIFF, "Orientation", "3"),
            ("urn:a", "x", "1"),
            ("urn:b", "x", "2"),
            ("urn:a", "z", "3"),
            ("urn:d", "k", "4"),
        ]
    );
}

#[test]
fn bom_and_leading_whitespace() {
    let mut p = vec![0xEFu8, 0xBB, 0xBF];
    p.extend(b"  \n");
    p.extend(packet("tiff:Orientation=\"4\"", ""));
    let x = parse(&p).unwrap();
    assert_eq!(x.orientation(), Some(4));
    assert_eq!(x.raw(), &p[..]);
}

#[test]
fn extended_packet_comes_last() {
    let base = packet("xmp:Rating=\"1\" tiff:Orientation=\"6\"", "");
    let ext = packet("dc:format=\"image/jpeg\"", "<xmp:Rating>5</xmp:Rating>");
    let x = Xmp::parse(&base, Some(ext.as_slice()), &lim(), &AtomicBool::new(false)).unwrap();
    assert_eq!(x.raw(), &base[..]);
    assert_eq!(
        props(&x),
        [
            (XMP, "Rating", "1"),
            (TIFF, "Orientation", "6"),
            (DC, "format", "image/jpeg"),
            (XMP, "Rating", "5"),
        ]
    );
    // The first Rating wins.
    assert_eq!(x.rating(), Some("1"));
}

#[test]
fn hdrgm_properties() {
    let p = packet(
        "hdrgm:Version=\"1.0\" hdrgm:GainMapMin=\"0\" hdrgm:GainMapMax=\"3.5\" xmp:Rating=\"2\"",
        "<hdrgm:Gamma>1.25</hdrgm:Gamma>",
    );
    let x = parse(&p).unwrap();
    let h: Vec<_> = x
        .hdrgm()
        .map(|p| (p.name.as_str(), p.value.as_str()))
        .collect();
    assert_eq!(
        h,
        [
            ("Version", "1.0"),
            ("GainMapMin", "0"),
            ("GainMapMax", "3.5"),
            ("Gamma", "1.25"),
        ]
    );
    assert!(x.hdrgm().all(|p| p.ns == HDR));
    assert_eq!(parse(&packet("", "")).unwrap().hdrgm().count(), 0);
}

#[test]
fn empty_and_invalid_utf8_values() {
    let p = packet("", "<dc:a/><dc:b></dc:b><dc:c x=\"1\"/><dc:d>ok</dc:d>");
    let x = parse(&p).unwrap();
    assert_eq!(props(&x), [(DC, "b", ""), (DC, "d", "ok")]);
    let mut p = packet("", "<dc:bad>QQ</dc:bad><dc:good>1</dc:good>");
    let at = p.windows(2).position(|w| w == b"QQ").unwrap();
    p[at] = 0xFF;
    p[at + 1] = b'x';
    let x = parse(&p).unwrap();
    assert_eq!(props(&x), [(DC, "good", "1")]);
}

#[test]
fn not_xml_is_malformed() {
    for p in [
        &b""[..],
        b"hello",
        b"  \xEF\xBB\xBF<a/>",
        b"\xFF\xFE<\0a\0/\0>\0",
    ] {
        assert_eq!(parse(p).unwrap_err(), Error::Malformed("xmp"), "{p:?}");
    }
    // Starts with `<` but has nothing in it: not an error, no properties.
    assert!(parse(b"<a").unwrap().properties().is_empty());
    assert!(parse(b"<<<>>>").unwrap().properties().is_empty());
}

#[test]
fn malformed_xml_keeps_what_was_parsed() {
    let p = packet("tiff:Orientation=\"6\"", "<dc:a>1</dc:a><dc:b ");
    let x = parse(&p).unwrap();
    assert_eq!(props(&x), [(TIFF, "Orientation", "6"), (DC, "a", "1")]);
    // Mismatched closing tags and stray text.
    let p = packet("", "text </nope> <dc:a>1</dc:zzz><dc:b>2</dc:b>");
    let x = parse(&p).unwrap();
    assert_eq!(x.get(DC, "b"), Some("2"));
}

#[test]
fn truncated_packets_never_panic() {
    let p = packet(
        "tiff:Orientation=\"6\" xmp:Rating='1'",
        "<dc:a><rdf:Seq><rdf:li>x&amp;<![CDATA[y]]></rdf:li></rdf:Seq></dc:a><!-- c -->",
    );
    for n in 0..=p.len() {
        let r = parse(&p[..n]);
        assert!(matches!(r, Ok(_) | Err(Error::Malformed(_))), "{n}: {r:?}");
    }
    for i in 0..p.len() {
        for v in [0u8, b'<', b'>', b'"', 0xFF] {
            let mut c = p.clone();
            c[i] = v;
            let r = parse(&c);
            assert!(matches!(r, Ok(_) | Err(Error::Malformed(_))), "{i}: {r:?}");
        }
    }
}

#[test]
fn many_siblings_deep_nesting_and_namespace_flood() {
    let body = "<dc:a>1</dc:a>".repeat(20_000);
    assert_eq!(
        parse(&packet("", &body)).unwrap().properties().len(),
        20_000
    );

    let body = "<a>".repeat(50_000);
    assert!(parse(&packet("", &body)).unwrap().properties().is_empty());

    let body: String = (0..300)
        .map(|i| format!("<a xmlns:p{i}=\"u{i}\">"))
        .collect();
    assert!(parse(&packet("", &body)).is_ok());
}

#[test]
fn limit_xmp_bytes_counts_both_packets() {
    let p = packet("tiff:Orientation=\"6\"", "");
    let ext = packet("xmp:Rating=\"1\"", "");
    let at = |max| Limits {
        max_xmp_bytes: max,
        ..lim()
    };
    let c = AtomicBool::new(false);
    assert!(Xmp::parse(&p, None, &at(p.len()), &c).is_ok());
    assert_eq!(
        Xmp::parse(&p, None, &at(p.len() - 1), &c).unwrap_err(),
        Error::LimitExceeded(Limit::XmpBytes)
    );
    let both = p.len() + ext.len();
    assert!(Xmp::parse(&p, Some(ext.as_slice()), &at(both), &c).is_ok());
    assert_eq!(
        Xmp::parse(&p, Some(ext.as_slice()), &at(both - 1), &c).unwrap_err(),
        Error::LimitExceeded(Limit::XmpBytes)
    );
}

#[test]
fn limit_xmp_nodes() {
    let body = "<dc:a>1</dc:a>".repeat(100);
    let p = packet("", &body);
    let at = |max| Limits {
        max_xmp_nodes: max,
        ..lim()
    };
    let c = AtomicBool::new(false);
    assert!(Xmp::parse(&p, None, &at(10_000), &c).is_ok());
    assert_eq!(
        Xmp::parse(&p, None, &at(50), &c).unwrap_err(),
        Error::LimitExceeded(Limit::XmpNodes)
    );
    // Attributes count as nodes.
    let attrs: String = (0..100).map(|i| format!("dc:a{i}=\"1\" ")).collect();
    let p = packet(&attrs, "");
    assert_eq!(
        Xmp::parse(&p, None, &at(50), &c).unwrap_err(),
        Error::LimitExceeded(Limit::XmpNodes)
    );
    // The extended packet has its own count.
    assert!(Xmp::parse(&p, Some(p.as_slice()), &at(10_000), &c).is_ok());
}

#[test]
fn limit_alloc() {
    let p = packet("tiff:Orientation=\"6\"", "");
    let c = AtomicBool::new(false);
    let small = Limits {
        max_alloc: 16,
        ..lim()
    };
    assert_eq!(
        Xmp::parse(&p, None, &small, &c).unwrap_err(),
        Error::LimitExceeded(Limit::Alloc)
    );
    // Enough for the copy but not for the stacks and values.
    let tight = Limits {
        max_alloc: p.len() + 8,
        ..lim()
    };
    assert_eq!(
        Xmp::parse(&p, None, &tight, &c).unwrap_err(),
        Error::LimitExceeded(Limit::Alloc)
    );
    // Deep nesting is charged too.
    let deep = packet("", &"<a>".repeat(50_000));
    let low = Limits {
        max_alloc: deep.len() + 1000,
        ..lim()
    };
    assert_eq!(
        Xmp::parse(&deep, None, &low, &c).unwrap_err(),
        Error::LimitExceeded(Limit::Alloc)
    );
}

#[test]
fn cancel_flag_set_before_the_call() {
    let p = packet("tiff:Orientation=\"6\"", "");
    let c = AtomicBool::new(true);
    assert_eq!(
        Xmp::parse(&p, None, &lim(), &c).unwrap_err(),
        Error::Cancelled
    );
}
