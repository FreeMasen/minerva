//! A tiny in-memory catalog used to drive the OPDS feeds.
//!
//! In a real server this would be backed by a database or a filesystem scan;
//! here it is a fixed set of books so the endpoints have something to serve.

use crate::model::*;

/// A single book in the catalog. This is the server's own domain type, kept
/// separate from the wire (`Publication`) representation.
#[derive(Debug, Clone)]
pub struct Book {
    pub id: &'static str,
    pub title: &'static str,
    pub author: &'static str,
    pub language: &'static str,
    pub description: &'static str,
    pub modified: &'static str,
    /// One of the top-level catalog categories (used for navigation/facets).
    pub category: Category,
    /// Price in USD, if the book is for sale rather than a free download.
    pub price_usd: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Fiction,
    NonFiction,
}

impl Category {
    pub fn slug(self) -> &'static str {
        match self {
            Category::Fiction => "fiction",
            Category::NonFiction => "nonfiction",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Category::Fiction => "Fiction",
            Category::NonFiction => "Non-Fiction",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Category> {
        match slug {
            "fiction" => Some(Category::Fiction),
            "nonfiction" => Some(Category::NonFiction),
            _ => None,
        }
    }
}

/// The full set of books served by this catalog.
pub fn books() -> Vec<Book> {
    vec![
        Book {
            id: "moby-dick",
            title: "Moby-Dick; or, The Whale",
            author: "Herman Melville",
            language: "en",
            description: "The saga of Captain Ahab and his monomaniacal pursuit of the white whale.",
            modified: "2015-09-29T17:00:00Z",
            category: Category::Fiction,
            price_usd: None,
        },
        Book {
            id: "pride-and-prejudice",
            title: "Pride and Prejudice",
            author: "Jane Austen",
            language: "en",
            description: "Elizabeth Bennet navigates manners, upbringing, and marriage.",
            modified: "2016-01-12T09:30:00Z",
            category: Category::Fiction,
            price_usd: Some(4.99),
        },
        Book {
            id: "frankenstein",
            title: "Frankenstein; or, The Modern Prometheus",
            author: "Mary Shelley",
            language: "en",
            description: "A scientist creates a sapient creature and reaps the consequences.",
            modified: "2018-07-03T12:00:00Z",
            category: Category::Fiction,
            price_usd: None,
        },
        Book {
            id: "on-the-origin-of-species",
            title: "On the Origin of Species",
            author: "Charles Darwin",
            language: "en",
            description: "The foundational work of evolutionary biology.",
            modified: "2017-11-24T00:00:00Z",
            category: Category::NonFiction,
            price_usd: Some(2.99),
        },
        Book {
            id: "the-art-of-war",
            title: "The Art of War",
            author: "Sun Tzu",
            language: "en",
            description: "An ancient Chinese treatise on military strategy.",
            modified: "2019-02-14T08:00:00Z",
            category: Category::NonFiction,
            price_usd: None,
        },
    ]
}

/// Look up a single book by its id.
pub fn book(id: &str) -> Option<Book> {
    books().into_iter().find(|b| b.id == id)
}

impl Book {
    /// Build the OPDS `Publication` (as embedded in a feed) for this book.
    pub fn to_publication(&self, base: &str) -> Publication {
        let mut metadata = Metadata::new(self.title);
        metadata.type_ = Some("http://schema.org/Book".to_string());
        metadata.identifier = Some(format!("urn:opds:book:{}", self.id));
        metadata.author = Some(Contributor::new(self.author));
        metadata.language = Some(self.language.to_string());
        metadata.description = Some(self.description.to_string());
        metadata.modified = Some(self.modified.to_string());

        let self_link = Link::new(format!("{base}/opds/publications/{}", self.id))
            .with_rel("self")
            .with_type(PUBLICATION_MEDIA_TYPE);

        // Acquisition link: a free download, or a paid "buy" link with a price.
        let acquisition = match self.price_usd {
            None => Link::new(format!("{base}/opds/download/{}.epub", self.id))
                .with_rel("http://opds-spec.org/acquisition/open-access")
                .with_type("application/epub+zip"),
            Some(price) => Link::new(format!("{base}/opds/buy/{}", self.id))
                .with_rel("http://opds-spec.org/acquisition/buy")
                .with_type("application/epub+zip")
                .with_properties(LinkProperties {
                    price: Some(Price {
                        currency: "USD".to_string(),
                        value: price,
                    }),
                    ..Default::default()
                }),
        };

        let cover = Link::new(format!("{base}/opds/covers/{}.svg", self.id))
            .with_rel("http://opds-spec.org/image")
            .with_type("image/svg+xml")
            .with_dimensions(800, 1200);

        let thumbnail = Link::new(format!("{base}/opds/covers/{}-thumb.svg", self.id))
            .with_rel("http://opds-spec.org/image/thumbnail")
            .with_type("image/svg+xml")
            .with_dimensions(160, 240);

        Publication {
            metadata,
            links: vec![self_link, acquisition],
            images: vec![cover, thumbnail],
        }
    }
}
