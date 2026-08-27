//! Data model for OPDS 2.0 feeds and publications.
//!
//! OPDS 2.0 is built on the Readium Web Publication Manifest model where
//! "everything is a collection" made up of `metadata`, `links` and
//! sub-collections (`publications`, `navigation`, `groups`, `facets`).
//!
//! See <https://specs.opds.io/opds-2.0.html>.

use serde::Serialize;

/// Media type for an OPDS 2.0 feed.
pub const FEED_MEDIA_TYPE: &str = "application/opds+json";
/// Media type for a single OPDS publication.
pub const PUBLICATION_MEDIA_TYPE: &str = "application/opds-publication+json";
/// Media type for an Authentication for OPDS document.
pub const AUTH_MEDIA_TYPE: &str = "application/opds-authentication+json";

/// An Authentication for OPDS document, describing how a client may
/// authenticate. Returned with 401 from protected resources and served
/// directly at a discoverable URL.
#[derive(Debug, Clone, Serialize)]
pub struct AuthenticationDocument {
    pub id: String,
    pub title: String,
    pub authentication: Vec<AuthenticationFlow>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
}

/// A single supported authentication flow.
#[derive(Debug, Clone, Serialize)]
pub struct AuthenticationFlow {
    /// The flow type URI, e.g. `http://opds-spec.org/auth/basic`.
    #[serde(rename = "type")]
    pub type_: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<AuthLabels>,
}

/// Human-facing labels for credential prompts.
#[derive(Debug, Clone, Serialize)]
pub struct AuthLabels {
    pub login: String,
    pub password: String,
}

/// A top-level OPDS feed (a Readium collection acting as a catalog).
///
/// A valid feed carries a `title` in its metadata, a `self` link, and at least
/// one of the `navigation`, `publications` or `groups` collections.
#[derive(Debug, Clone, Serialize)]
pub struct Feed {
    pub metadata: Metadata,
    pub links: Vec<Link>,

    /// Ordered links used to browse the catalog (a compact collection).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub navigation: Vec<Link>,

    /// A list of publications, e.g. an acquisition feed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub publications: Vec<Publication>,

    /// Facets: alternate views, filters or sort options for the feed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub facets: Vec<Facet>,

    /// Groups bundle several navigation/publication collections in one feed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<Group>,
}

impl Feed {
    /// Create a feed with the mandatory title and self link.
    pub fn new(title: impl Into<String>, self_href: impl Into<String>) -> Self {
        Feed {
            metadata: Metadata::new(title),
            links: vec![Link::self_link(self_href, FEED_MEDIA_TYPE)],
            navigation: Vec::new(),
            publications: Vec::new(),
            facets: Vec::new(),
            groups: Vec::new(),
        }
    }

    pub fn with_link(mut self, link: Link) -> Self {
        self.links.push(link);
        self
    }
}

/// Metadata for a collection or publication.
///
/// Only a small, commonly-used subset of the Readium metadata vocabulary is
/// modelled here. Unknown/extra fields could be added as needed.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Metadata {
    /// The schema.org type, serialized as `@type`, e.g. `http://schema.org/Book`.
    #[serde(rename = "@type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,

    pub title: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<Contributor>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<Contributor>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Last modification date (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,

    // --- Pagination metadata (used on feeds) ---
    #[serde(rename = "numberOfItems", skip_serializing_if = "Option::is_none")]
    pub number_of_items: Option<u64>,

    #[serde(rename = "itemsPerPage", skip_serializing_if = "Option::is_none")]
    pub items_per_page: Option<u64>,

    #[serde(rename = "currentPage", skip_serializing_if = "Option::is_none")]
    pub current_page: Option<u64>,
}

impl Metadata {
    pub fn new(title: impl Into<String>) -> Self {
        Metadata {
            title: title.into(),
            ..Default::default()
        }
    }
}

/// A contributor (author, publisher, ...). May be a bare name or carry links.
#[derive(Debug, Clone, Serialize)]
pub struct Contributor {
    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
}

impl Contributor {
    pub fn new(name: impl Into<String>) -> Self {
        Contributor {
            name: name.into(),
            identifier: None,
            links: Vec::new(),
        }
    }
}

/// A Link Object. `rel` and `type` are optional per the spec.
#[derive(Debug, Clone, Serialize)]
pub struct Link {
    pub href: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub rel: Option<Rel>,

    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// Whether `href` is a URI template (e.g. contains `{?query}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub templated: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<LinkProperties>,

    /// Optional image dimensions (used in the `images` collection).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
}

impl Link {
    pub fn new(href: impl Into<String>) -> Self {
        Link {
            href: href.into(),
            rel: None,
            type_: None,
            title: None,
            templated: None,
            properties: None,
            height: None,
            width: None,
        }
    }

    pub fn self_link(href: impl Into<String>, media_type: &str) -> Self {
        Link::new(href).with_rel("self").with_type(media_type)
    }

    pub fn with_rel(mut self, rel: impl Into<String>) -> Self {
        self.rel = Some(Rel::One(rel.into()));
        self
    }

    pub fn with_type(mut self, media_type: impl Into<String>) -> Self {
        self.type_ = Some(media_type.into());
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn templated(mut self) -> Self {
        self.templated = Some(true);
        self
    }

    pub fn with_properties(mut self, properties: LinkProperties) -> Self {
        self.properties = Some(properties);
        self
    }

    pub fn with_dimensions(mut self, width: u32, height: u32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }
}

/// A relation can be a single value or an array of values.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Rel {
    One(String),
    /// Multiple relations on one link. Part of the model for completeness; no
    /// current link needs more than one relation.
    #[allow(dead_code)]
    Many(Vec<String>),
}

/// Additional metadata attached to a link, e.g. acquisition details.
#[derive(Debug, Clone, Serialize, Default)]
pub struct LinkProperties {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,

    #[serde(rename = "numberOfItems", skip_serializing_if = "Option::is_none")]
    pub number_of_items: Option<u64>,

    /// Description of the format obtained after an intermediate acquisition step.
    #[serde(
        rename = "indirectAcquisition",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub indirect_acquisition: Vec<IndirectAcquisition>,

    // --- Library lending (an OPDS extension, not part of core OPDS 2.0) ---
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability: Option<Availability>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub holds: Option<Holds>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub copies: Option<Copies>,
}

/// Lending availability of a resource.
#[derive(Debug, Clone, Serialize)]
pub struct Availability {
    /// One of `available`, `unavailable`, `reserved`, `ready`.
    pub state: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

/// Hold (reservation) counts for a lendable resource.
#[derive(Debug, Clone, Serialize)]
pub struct Holds {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<u64>,
}

/// Copy counts for a lendable resource.
#[derive(Debug, Clone, Serialize)]
pub struct Copies {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub available: Option<u64>,
}

/// A price with an ISO 4217 currency code and a decimal value.
#[derive(Debug, Clone, Serialize)]
pub struct Price {
    pub currency: String,
    pub value: f64,
}

/// The format reached after an indirect acquisition step, nestable.
#[derive(Debug, Clone, Serialize)]
pub struct IndirectAcquisition {
    #[serde(rename = "type")]
    pub type_: String,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub child: Vec<IndirectAcquisition>,
}

/// A publication: metadata, links (with at least one acquisition link) and
/// optional cover images.
#[derive(Debug, Clone, Serialize)]
pub struct Publication {
    pub metadata: Metadata,
    pub links: Vec<Link>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<Link>,
}

/// A facet groups links that filter or sort a feed.
#[derive(Debug, Clone, Serialize)]
pub struct Facet {
    pub metadata: Metadata,
    pub links: Vec<Link>,
}

/// A group bundles a sub-collection (navigation or publications) with its own
/// metadata and links.
#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub metadata: Metadata,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub navigation: Vec<Link>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub publications: Vec<Publication>,
}
