//! The OPDS 1.2 wire format: Atom feeds and entries.
//!
//! OPDS 1.x predates the Readium collection model of OPDS 2.0 ([`crate::model`]):
//! a catalog is an Atom feed, either a *navigation* feed whose entries link to
//! further feeds or an *acquisition* feed whose entries are publications. The
//! OPDS-specific vocabulary (prices, indirect acquisition, lending
//! availability) rides along in the `opds:` namespace as child elements of a
//! link, where OPDS 2.0 instead nests a JSON `properties` object — so the
//! vocabulary types themselves ([`LinkProperties`] and friends) are shared with
//! the 2.0 model and only their rendering differs.
//!
//! See <https://specs.opds.io/opds-1.2>.

use std::borrow::Cow;
use std::io::Write;

use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};

use crate::model::{IndirectAcquisition, LinkProperties};

/// Media type of an OPDS 1.x navigation feed.
pub const NAVIGATION_MEDIA_TYPE: &str =
    "application/atom+xml;profile=opds-catalog;kind=navigation";
/// Media type of an OPDS 1.x acquisition feed.
pub const ACQUISITION_MEDIA_TYPE: &str =
    "application/atom+xml;profile=opds-catalog;kind=acquisition";
/// Media type of a single OPDS 1.x entry ("complete entry") document.
pub const ENTRY_MEDIA_TYPE: &str = "application/atom+xml;type=entry;profile=opds-catalog";
/// Media type of an OpenSearch description document.
pub const OPENSEARCH_MEDIA_TYPE: &str = "application/opensearchdescription+xml";

/// The `scheme` advertised on an entry's `<category>` elements.
pub const CATEGORY_SCHEME: &str = "http://opds-spec.org/2010/catalog/category";

// Namespaces declared on every feed (or standalone entry) root.
const NAMESPACES: [(&str, &str); 6] = [
    ("xmlns", "http://www.w3.org/2005/Atom"),
    ("xmlns:opds", "http://opds-spec.org/2010/catalog"),
    ("xmlns:dcterms", "http://purl.org/dc/terms/"),
    ("xmlns:opensearch", "http://a9.com/-/spec/opensearch/1.1/"),
    ("xmlns:thr", "http://purl.org/syndication/thread/1.0"),
    ("xmlns:schema", "http://schema.org"),
];

/// Which of the two OPDS 1.x feed kinds a feed is. The distinction is carried
/// only by the media type: clients use it to decide whether to render entries
/// as browsable links or as acquirable publications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedKind {
    Navigation,
    Acquisition,
}

impl FeedKind {
    pub fn media_type(self) -> &'static str {
        match self {
            FeedKind::Navigation => NAVIGATION_MEDIA_TYPE,
            FeedKind::Acquisition => ACQUISITION_MEDIA_TYPE,
        }
    }
}

/// An Atom feed carrying an OPDS catalog.
#[derive(Debug, Clone)]
pub struct Feed {
    pub kind: FeedKind,
    pub id: String,
    pub title: String,
    /// Atom requires a feed-level `updated`.
    pub updated: jiff::Timestamp,
    pub subtitle: Option<String>,
    pub links: Vec<Link>,
    pub entries: Vec<Entry>,
    /// OpenSearch result counts, on feeds that are a page of a larger set.
    pub counts: Option<Counts>,
}

impl Feed {
    /// A feed with the mandatory id/title/updated and a `self` link of the
    /// kind's own media type (clients reject a feed whose `self` link
    /// disagrees with the response's content type).
    pub fn new(kind: FeedKind, id: impl Into<String>, title: impl Into<String>) -> Self {
        let id = id.into();
        let self_link = Link::new(id.clone(), "self", kind.media_type());
        Feed {
            kind,
            id,
            title: title.into(),
            updated: jiff::Timestamp::now(),
            subtitle: None,
            links: vec![self_link],
            entries: Vec::new(),
            counts: None,
        }
    }

    pub fn with_link(mut self, link: Link) -> Self {
        self.links.push(link);
        self
    }

    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Serialize the feed as an XML document.
    pub fn to_xml(&self) -> Vec<u8> {
        write_document("feed", |w| {
            text_element(w, "id", &self.id)?;
            text_element(w, "title", &self.title)?;
            text_element(w, "updated", &self.updated.to_string())?;
            if let Some(subtitle) = &self.subtitle {
                text_element(w, "subtitle", subtitle)?;
            }
            if let Some(counts) = &self.counts {
                counts.write(w)?;
            }
            for link in &self.links {
                link.write(w)?;
            }
            for entry in &self.entries {
                entry.write(w)?;
            }
            Ok(())
        })
    }
}

/// OpenSearch result counts describing a feed's place in a larger result set.
/// `start_index` is 1-based, per the OpenSearch specification.
#[derive(Debug, Clone, Copy)]
pub struct Counts {
    pub total_results: u64,
    pub items_per_page: u64,
    pub start_index: u64,
}

impl Counts {
    fn write<W: Write>(&self, w: &mut Writer<W>) -> std::io::Result<()> {
        text_element(w, "opensearch:totalResults", &self.total_results.to_string())?;
        text_element(w, "opensearch:itemsPerPage", &self.items_per_page.to_string())?;
        text_element(w, "opensearch:startIndex", &self.start_index.to_string())
    }
}

/// An Atom entry: a publication in an acquisition feed, or a link to a further
/// feed in a navigation feed.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub updated: jiff::Timestamp,
    pub author: Option<String>,
    pub language: Option<String>,
    /// A short plain-text description. Clients read `summary` more reliably
    /// than `content`, so acquisition feeds carry it on every entry.
    pub summary: Option<String>,
    /// The full description, as HTML. Reserved for complete-entry documents.
    pub content: Option<String>,
    /// The series this entry belongs to, and its position within it.
    pub series: Option<(String, Option<f64>)>,
    pub categories: Vec<Category>,
    pub links: Vec<Link>,
}

impl Entry {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Entry {
            id: id.into(),
            title: title.into(),
            updated: jiff::Timestamp::now(),
            author: None,
            language: None,
            summary: None,
            content: None,
            series: None,
            categories: Vec::new(),
            links: Vec::new(),
        }
    }

    pub fn with_link(mut self, link: Link) -> Self {
        self.links.push(link);
        self
    }

    /// Serialize the entry as a standalone "complete entry" document.
    pub fn to_xml(&self) -> Vec<u8> {
        write_document("entry", |w| self.write_children(w))
    }

    /// Write the entry nested inside a feed (namespaces live on the feed root).
    fn write<W: Write>(&self, w: &mut Writer<W>) -> std::io::Result<()> {
        w.write_event(Event::Start(BytesStart::new("entry")))?;
        self.write_children(w)?;
        w.write_event(Event::End(BytesEnd::new("entry")))
    }

    fn write_children<W: Write>(&self, w: &mut Writer<W>) -> std::io::Result<()> {
        text_element(w, "id", &self.id)?;
        text_element(w, "title", &self.title)?;
        text_element(w, "updated", &self.updated.to_string())?;
        if let Some(author) = &self.author {
            w.create_element("author")
                .write_inner_content(|w| text_element(w, "name", author))?;
        }
        if let Some(language) = &self.language {
            text_element(w, "dcterms:language", language)?;
        }
        if let Some(summary) = &self.summary {
            w.create_element("summary")
                .with_attribute(("type", "text"))
                .write_text_content(BytesText::new(summary))?;
        }
        if let Some(content) = &self.content {
            w.create_element("content")
                .with_attribute(("type", "html"))
                .write_text_content(BytesText::new(content))?;
        }
        if let Some((name, position)) = &self.series {
            let mut series = BytesStart::new("schema:Series");
            series.push_attribute(("name", name.as_str()));
            let position = position.map(format_position);
            if let Some(position) = &position {
                series.push_attribute(("position", position.as_str()));
            }
            w.write_event(Event::Empty(series))?;
        }
        for category in &self.categories {
            category.write(w)?;
        }
        for link in &self.links {
            link.write(w)?;
        }
        Ok(())
    }
}

/// A category an entry belongs to.
#[derive(Debug, Clone)]
pub struct Category {
    pub term: String,
    pub label: String,
}

impl Category {
    fn write<W: Write>(&self, w: &mut Writer<W>) -> std::io::Result<()> {
        w.create_element("category")
            .with_attribute(("scheme", CATEGORY_SCHEME))
            .with_attribute(("term", self.term.as_str()))
            .with_attribute(("label", self.label.as_str()))
            .write_empty()?;
        Ok(())
    }
}

/// An Atom link. OPDS 1.x hangs its extension vocabulary off links as child
/// elements, so `properties` is the same type the 2.0 model serializes as a
/// JSON object.
#[derive(Debug, Clone)]
pub struct Link {
    pub href: String,
    pub rel: Cow<'static, str>,
    pub r#type: Cow<'static, str>,
    pub title: Option<String>,
    /// The facet group this link belongs to (`opds:facetGroup`). OPDS 1.x
    /// groups facets by this attribute where 2.0 nests them structurally.
    pub facet_group: Option<Cow<'static, str>>,
    /// Whether this facet is the one currently applied (`opds:activeFacet`).
    pub active_facet: bool,
    pub properties: Option<LinkProperties>,
}

impl Link {
    pub fn new(
        href: impl Into<String>,
        rel: impl Into<Cow<'static, str>>,
        media_type: impl Into<Cow<'static, str>>,
    ) -> Self {
        Link {
            href: href.into(),
            rel: rel.into(),
            r#type: media_type.into(),
            title: None,
            facet_group: None,
            active_facet: false,
            properties: None,
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_properties(mut self, properties: LinkProperties) -> Self {
        self.properties = Some(properties);
        self
    }

    /// Mark this link as a facet in `group`, optionally the active one.
    pub fn with_facet(mut self, group: impl Into<Cow<'static, str>>, active: bool) -> Self {
        self.facet_group = Some(group.into());
        self.active_facet = active;
        self
    }

    fn write<W: Write>(&self, w: &mut Writer<W>) -> std::io::Result<()> {
        let mut el = BytesStart::new("link");
        el.push_attribute(("href", self.href.as_str()));
        el.push_attribute(("rel", self.rel.as_ref()));
        el.push_attribute(("type", self.r#type.as_ref()));
        if let Some(title) = &self.title {
            el.push_attribute(("title", title.as_str()));
        }
        if let Some(group) = &self.facet_group {
            el.push_attribute(("opds:facetGroup", group.as_ref()));
        }
        if self.active_facet {
            el.push_attribute(("opds:activeFacet", "true"));
        }
        // A 2.0 facet counts its results in `properties.numberOfItems`; the
        // 1.x spelling is the Atom threading extension's `thr:count`.
        let count = self
            .properties
            .as_ref()
            .and_then(|p| p.number_of_items)
            .map(|n| n.to_string());
        if let Some(count) = &count {
            el.push_attribute(("thr:count", count.as_str()));
        }

        let children = self.properties.as_ref().filter(|p| {
            p.price.is_some()
                || !p.indirect_acquisition.is_empty()
                || p.availability.is_some()
                || p.copies.is_some()
                || p.holds.is_some()
        });
        let Some(props) = children else {
            return w.write_event(Event::Empty(el));
        };

        w.write_event(Event::Start(el))?;
        if let Some(price) = &props.price {
            w.create_element("opds:price")
                .with_attribute(("currencycode", price.currency.as_ref()))
                .write_text_content(BytesText::new(&format!("{:.2}", price.value)))?;
        }
        for indirect in &props.indirect_acquisition {
            write_indirect(w, indirect)?;
        }
        if let Some(availability) = &props.availability {
            let mut el = BytesStart::new("opds:availability");
            el.push_attribute(("status", availability.state.as_str()));
            if let Some(since) = &availability.since {
                el.push_attribute(("since", since.as_str()));
            }
            if let Some(until) = &availability.until {
                el.push_attribute(("until", until.as_str()));
            }
            w.write_event(Event::Empty(el))?;
        }
        if let Some(copies) = &props.copies {
            write_counted(w, "opds:copies", copies.total, "available", copies.available)?;
        }
        if let Some(holds) = &props.holds {
            write_counted(w, "opds:holds", holds.total, "position", holds.position)?;
        }
        w.write_event(Event::End(BytesEnd::new("link")))
    }
}

/// An OpenSearch description document, advertising the search endpoint's URL
/// template. OPDS 1.x discovers search this way rather than with 2.0's
/// templated link.
#[derive(Debug, Clone)]
pub struct OpenSearchDescription {
    pub short_name: String,
    pub description: String,
    /// The URL template, with an `{searchTerms}` placeholder.
    pub template: String,
}

impl OpenSearchDescription {
    pub fn to_xml(&self) -> Vec<u8> {
        let mut w = Writer::new(Vec::new());
        let write = |w: &mut Writer<Vec<u8>>| -> std::io::Result<()> {
            w.write_event(Event::Decl(BytesDecl::new("1.0", Some("utf-8"), None)))?;
            let mut root = BytesStart::new("OpenSearchDescription");
            root.push_attribute(("xmlns", "http://a9.com/-/spec/opensearch/1.1/"));
            w.write_event(Event::Start(root))?;
            text_element(w, "ShortName", &self.short_name)?;
            text_element(w, "Description", &self.description)?;
            text_element(w, "InputEncoding", "UTF-8")?;
            text_element(w, "OutputEncoding", "UTF-8")?;
            w.create_element("Url")
                .with_attribute(("type", ACQUISITION_MEDIA_TYPE))
                .with_attribute(("template", self.template.as_str()))
                .write_empty()?;
            w.write_event(Event::End(BytesEnd::new("OpenSearchDescription")))
        };
        write(&mut w).expect("writing XML into a Vec cannot fail");
        w.into_inner()
    }
}

/// Write a complete XML document whose root element is `root` (carrying the
/// OPDS namespace declarations), with `children` writing its contents.
fn write_document<F>(root: &str, children: F) -> Vec<u8>
where
    F: FnOnce(&mut Writer<Vec<u8>>) -> std::io::Result<()>,
{
    let mut w = Writer::new(Vec::new());
    let write = |w: &mut Writer<Vec<u8>>| -> std::io::Result<()> {
        w.write_event(Event::Decl(BytesDecl::new("1.0", Some("utf-8"), None)))?;
        let mut el = BytesStart::new(root);
        el.extend_attributes(NAMESPACES);
        w.write_event(Event::Start(el))?;
        children(w)?;
        w.write_event(Event::End(BytesEnd::new(root)))
    };
    // Writing into a `Vec` cannot fail, so the whole wire layer stays infallible.
    write(&mut w).expect("writing XML into a Vec cannot fail");
    w.into_inner()
}

/// Write `<name>text</name>`, escaping `text`.
fn text_element<W: Write>(w: &mut Writer<W>, name: &str, text: &str) -> std::io::Result<()> {
    w.create_element(name)
        .write_text_content(BytesText::new(text))?;
    Ok(())
}

/// Write an `opds:indirectAcquisition` element and, recursively, its children.
fn write_indirect<W: Write>(
    w: &mut Writer<W>,
    indirect: &IndirectAcquisition,
) -> std::io::Result<()> {
    let el = w
        .create_element("opds:indirectAcquisition")
        .with_attribute(("type", indirect.r#type.as_ref()));
    if indirect.child.is_empty() {
        el.write_empty()?;
    } else {
        el.write_inner_content(|w| {
            for child in &indirect.child {
                write_indirect(w, child)?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// Write an element carrying a `total` plus one other optional count, the
/// shape shared by `opds:copies` and `opds:holds`.
fn write_counted<W: Write>(
    w: &mut Writer<W>,
    name: &str,
    total: Option<u64>,
    other_name: &str,
    other: Option<u64>,
) -> std::io::Result<()> {
    let mut el = BytesStart::new(name);
    let total = total.map(|n| n.to_string());
    if let Some(total) = &total {
        el.push_attribute(("total", total.as_str()));
    }
    let other = other.map(|n| n.to_string());
    if let Some(other) = &other {
        el.push_attribute((other_name, other.as_str()));
    }
    w.write_event(Event::Empty(el))
}

/// Format a series position, dropping the fractional part of whole numbers
/// (clients display a bare "2" better than "2.0").
fn format_position(position: f64) -> String {
    if position.fract() == 0.0 && position.abs() < 1e15 {
        format!("{}", position as i64)
    } else {
        format!("{position}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Availability, AvailabilityState, Copies, Holds, Price};
    use roxmltree::{Document, Node};

    const NS_OPDS: &str = "http://opds-spec.org/2010/catalog";

    fn parse(xml: &[u8]) -> String {
        String::from_utf8(xml.to_vec()).expect("XML is valid UTF-8")
    }

    /// The first descendant with the given local name.
    fn find<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Node<'a, 'i> {
        node.descendants()
            .find(|n| n.is_element() && n.tag_name().name() == name)
            .unwrap_or_else(|| panic!("no <{name}> element in the document"))
    }

    fn text(node: Node<'_, '_>, name: &str) -> String {
        find(node, name).text().unwrap_or_default().to_string()
    }

    /// Every link in the document, as (rel, href) pairs.
    fn links(root: Node<'_, '_>) -> Vec<(String, String)> {
        root.descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "link")
            .map(|n| {
                (
                    n.attribute("rel").unwrap_or_default().to_string(),
                    n.attribute("href").unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    fn sample_feed() -> Feed {
        let mut feed = Feed::new(
            FeedKind::Acquisition,
            "http://example.test/opds1/all",
            "All Publications",
        );
        feed.entries.push(Entry::new("urn:opds:book:moby-dick", "Moby-Dick"));
        feed
    }

    #[test]
    fn feed_carries_the_required_atom_elements() {
        let xml = parse(&sample_feed().to_xml());
        let doc = Document::parse(&xml).expect("a well-formed feed");
        let root = doc.root_element();

        assert_eq!(root.tag_name().name(), "feed");
        assert_eq!(root.tag_name().namespace(), Some("http://www.w3.org/2005/Atom"));
        assert_eq!(text(root, "id"), "http://example.test/opds1/all");
        assert_eq!(text(root, "title"), "All Publications");
        // Atom requires `updated`, and it must be an RFC 3339 timestamp.
        let updated = text(root, "updated");
        assert!(
            updated.parse::<jiff::Timestamp>().is_ok(),
            "updated {updated:?} is not RFC 3339"
        );

        // An entry carries the same required trio.
        let entry = find(root, "entry");
        assert_eq!(text(entry, "id"), "urn:opds:book:moby-dick");
        assert_eq!(text(entry, "title"), "Moby-Dick");
        assert!(text(entry, "updated").parse::<jiff::Timestamp>().is_ok());
    }

    #[test]
    fn self_link_type_matches_the_feed_kind() {
        let navigation = parse(&Feed::new(FeedKind::Navigation, "http://x.test/opds1", "Root").to_xml());
        let doc = Document::parse(&navigation).unwrap();
        let self_link = doc
            .root_element()
            .children()
            .find(|n| n.attribute("rel") == Some("self"))
            .expect("a self link");
        assert_eq!(self_link.attribute("type"), Some(NAVIGATION_MEDIA_TYPE));

        let acquisition = parse(&sample_feed().to_xml());
        let doc = Document::parse(&acquisition).unwrap();
        let self_link = doc
            .root_element()
            .children()
            .find(|n| n.attribute("rel") == Some("self"))
            .expect("a self link");
        assert_eq!(self_link.attribute("type"), Some(ACQUISITION_MEDIA_TYPE));
    }

    // Titles and descriptions come from EPUB metadata, so the writer must
    // escape them: unescaped markup would produce an unparseable feed.
    #[test]
    fn text_and_attributes_are_escaped() {
        let nasty = r#"Tom & Jerry <b>"best"</b> 'ever'"#;
        let mut feed = Feed::new(FeedKind::Acquisition, "http://x.test/f", nasty);
        let mut entry = Entry::new("urn:x", nasty);
        entry.summary = Some(nasty.to_string());
        entry.author = Some(nasty.to_string());
        entry.links.push(Link::new(
            "http://x.test/search?query=a&b=c<d>",
            "alternate",
            "text/html",
        ));
        feed.entries.push(entry);

        let xml = parse(&feed.to_xml());
        assert!(!xml.contains("<b>"), "raw markup leaked into the feed: {xml}");
        let doc = Document::parse(&xml).expect("escaped content parses");
        let root = doc.root_element();

        // Round-tripping recovers the original text exactly.
        assert_eq!(text(root, "title"), nasty);
        assert_eq!(text(root, "summary"), nasty);
        assert_eq!(text(root, "name"), nasty);
        let entry = find(root, "entry");
        let alternate = entry
            .children()
            .find(|n| n.attribute("rel") == Some("alternate"))
            .expect("the alternate link");
        assert_eq!(
            alternate.attribute("href"),
            Some("http://x.test/search?query=a&b=c<d>")
        );
    }

    #[test]
    fn a_link_without_properties_is_an_empty_element() {
        let mut feed = sample_feed();
        feed.entries[0].links.push(Link::new(
            "http://x.test/opds/download/moby-dick/epub",
            "http://opds-spec.org/acquisition/open-access",
            "application/epub+zip",
        ));
        let xml = parse(&feed.to_xml());
        let doc = Document::parse(&xml).unwrap();
        let link = doc
            .root_element()
            .descendants()
            .find(|n| n.attribute("rel") == Some("http://opds-spec.org/acquisition/open-access"))
            .expect("the acquisition link");
        assert!(!link.has_children());
        assert_eq!(link.attribute("type"), Some("application/epub+zip"));
    }

    #[test]
    fn buy_link_renders_price_and_indirect_acquisition() {
        let mut feed = sample_feed();
        feed.entries[0].links.push(
            Link::new(
                "http://x.test/opds/buy/pride",
                "http://opds-spec.org/acquisition/buy",
                "text/html",
            )
            .with_properties(LinkProperties {
                price: Some(Price {
                    currency: "USD".into(),
                    value: 4.99,
                }),
                indirect_acquisition: vec![IndirectAcquisition {
                    r#type: "application/vnd.adobe.adept+xml".into(),
                    child: vec![IndirectAcquisition {
                        r#type: "application/epub+zip".into(),
                        child: Vec::new(),
                    }],
                }],
                ..Default::default()
            }),
        );

        let xml = parse(&feed.to_xml());
        let doc = Document::parse(&xml).unwrap();
        let root = doc.root_element();

        let price = find(root, "price");
        assert_eq!(price.tag_name().namespace(), Some(NS_OPDS));
        assert_eq!(price.attribute("currencycode"), Some("USD"));
        assert_eq!(price.text(), Some("4.99"));

        // Indirect acquisition nests: the outer step yields the inner format.
        let outer = find(root, "indirectAcquisition");
        assert_eq!(
            outer.attribute("type"),
            Some("application/vnd.adobe.adept+xml")
        );
        let inner = outer
            .children()
            .find(|n| n.is_element())
            .expect("a nested indirectAcquisition");
        assert_eq!(inner.tag_name().name(), "indirectAcquisition");
        assert_eq!(inner.attribute("type"), Some("application/epub+zip"));
    }

    #[test]
    fn borrow_link_renders_availability_copies_and_holds() {
        let mut feed = sample_feed();
        feed.entries[0].links.push(
            Link::new(
                "http://x.test/opds/borrow/art-of-war",
                "http://opds-spec.org/acquisition/borrow",
                "text/html",
            )
            .with_properties(LinkProperties {
                availability: Some(Availability {
                    state: AvailabilityState::Available,
                    since: None,
                    until: None,
                }),
                copies: Some(Copies {
                    total: Some(3),
                    available: Some(2),
                }),
                holds: Some(Holds {
                    total: Some(1),
                    position: None,
                }),
                ..Default::default()
            }),
        );

        let xml = parse(&feed.to_xml());
        let doc = Document::parse(&xml).unwrap();
        let root = doc.root_element();

        assert_eq!(find(root, "availability").attribute("status"), Some("available"));
        let copies = find(root, "copies");
        assert_eq!(copies.attribute("total"), Some("3"));
        assert_eq!(copies.attribute("available"), Some("2"));
        let holds = find(root, "holds");
        assert_eq!(holds.attribute("total"), Some("1"));
        // An absent count is omitted rather than written as an empty attribute.
        assert_eq!(holds.attribute("position"), None);
    }

    #[test]
    fn facet_link_carries_its_group_and_count() {
        let feed = sample_feed().with_link(
            Link::new(
                "http://x.test/opds1/category/fiction",
                "http://opds-spec.org/facet",
                ACQUISITION_MEDIA_TYPE,
            )
            .with_title("Fiction")
            .with_facet("Category", true)
            .with_properties(LinkProperties {
                number_of_items: Some(12),
                ..Default::default()
            }),
        );

        let xml = parse(&feed.to_xml());
        let doc = Document::parse(&xml).unwrap();
        let facet = doc
            .root_element()
            .children()
            .find(|n| n.attribute("rel") == Some("http://opds-spec.org/facet"))
            .expect("the facet link");

        assert_eq!(facet.attribute("title"), Some("Fiction"));
        assert_eq!(facet.attribute((NS_OPDS, "facetGroup")), Some("Category"));
        assert_eq!(facet.attribute((NS_OPDS, "activeFacet")), Some("true"));
        assert_eq!(
            facet.attribute(("http://purl.org/syndication/thread/1.0", "count")),
            Some("12")
        );
        // `numberOfItems` is a facet count, not acquisition detail: it must not
        // force the link open as a parent element.
        assert!(!facet.has_children());
    }

    #[test]
    fn counts_render_as_opensearch_elements() {
        let mut feed = sample_feed();
        feed.counts = Some(Counts {
            total_results: 137,
            items_per_page: 25,
            start_index: 26,
        });
        let xml = parse(&feed.to_xml());
        let doc = Document::parse(&xml).unwrap();
        let root = doc.root_element();

        assert_eq!(text(root, "totalResults"), "137");
        assert_eq!(text(root, "itemsPerPage"), "25");
        assert_eq!(text(root, "startIndex"), "26");
        assert_eq!(
            find(root, "totalResults").tag_name().namespace(),
            Some("http://a9.com/-/spec/opensearch/1.1/")
        );
    }

    #[test]
    fn entry_document_stands_alone_with_its_own_namespaces() {
        let mut entry = Entry::new("urn:opds:book:moby-dick", "Moby-Dick");
        entry.content = Some("<p>A whale.</p>".to_string());
        entry.language = Some("en".to_string());
        entry.categories.push(Category {
            term: "fiction".to_string(),
            label: "Fiction".to_string(),
        });
        let entry = entry.with_link(Link::new(
            "http://x.test/opds1/publications/moby-dick",
            "self",
            ENTRY_MEDIA_TYPE,
        ));

        let xml = parse(&entry.to_xml());
        let doc = Document::parse(&xml).expect("a well-formed entry document");
        let root = doc.root_element();

        assert_eq!(root.tag_name().name(), "entry");
        assert_eq!(root.tag_name().namespace(), Some("http://www.w3.org/2005/Atom"));
        // The description is HTML, carried as escaped text.
        let content = find(root, "content");
        assert_eq!(content.attribute("type"), Some("html"));
        assert_eq!(content.text(), Some("<p>A whale.</p>"));
        assert_eq!(
            find(root, "language").tag_name().namespace(),
            Some("http://purl.org/dc/terms/")
        );
        let category = find(root, "category");
        assert_eq!(category.attribute("term"), Some("fiction"));
        assert_eq!(category.attribute("label"), Some("Fiction"));
        assert_eq!(category.attribute("scheme"), Some(CATEGORY_SCHEME));
        assert_eq!(
            links(root),
            vec![(
                "self".to_string(),
                "http://x.test/opds1/publications/moby-dick".to_string()
            )]
        );
    }

    #[test]
    fn series_position_drops_a_whole_number_fraction() {
        let mut entry = Entry::new("urn:x", "Book Two");
        entry.series = Some(("The Trilogy".to_string(), Some(2.0)));
        let xml = parse(&entry.to_xml());
        let doc = Document::parse(&xml).unwrap();
        let series = find(doc.root_element(), "Series");
        assert_eq!(series.tag_name().namespace(), Some("http://schema.org"));
        assert_eq!(series.attribute("name"), Some("The Trilogy"));
        assert_eq!(series.attribute("position"), Some("2"));

        // A fractional position (a novella between two books) is preserved.
        let mut entry = Entry::new("urn:x", "Interlude");
        entry.series = Some(("The Trilogy".to_string(), Some(2.5)));
        let xml = parse(&entry.to_xml());
        let doc = Document::parse(&xml).unwrap();
        assert_eq!(
            find(doc.root_element(), "Series").attribute("position"),
            Some("2.5")
        );
    }

    #[test]
    fn opensearch_description_advertises_the_template() {
        let description = OpenSearchDescription {
            short_name: "Minerva".to_string(),
            description: "Search the catalog".to_string(),
            template: "http://x.test/opds1/search?query={searchTerms}".to_string(),
        };
        let xml = parse(&description.to_xml());
        let doc = Document::parse(&xml).expect("a well-formed description");
        let root = doc.root_element();

        assert_eq!(root.tag_name().name(), "OpenSearchDescription");
        assert_eq!(text(root, "ShortName"), "Minerva");
        let url = find(root, "Url");
        assert_eq!(url.attribute("type"), Some(ACQUISITION_MEDIA_TYPE));
        assert_eq!(
            url.attribute("template"),
            Some("http://x.test/opds1/search?query={searchTerms}")
        );
    }
}
