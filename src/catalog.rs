//! The catalog: the set of publications the server exposes.
//!
//! A [`Catalog`] is either the built-in [`Catalog::sample`] set (served when no
//! library directory is configured) or the result of scanning a directory of
//! EPUB files with [`Catalog::from_dir`]. Handlers see the same types either
//! way; only the [`BookSource`] differs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::epub::{self, CoverRef};
use crate::model::*;

/// A single book in the catalog. This is the server's own domain type, kept
/// separate from the wire (`Publication`) representation.
#[derive(Debug, Clone)]
pub struct Book {
    pub id: String,
    pub title: String,
    pub author: String,
    pub language: Option<String>,
    pub description: Option<String>,
    pub modified: Option<String>,
    /// One of the top-level catalog categories (used for navigation/facets).
    pub category: Category,
    /// Price in USD for a paid title, or `None` for a free/open-access download.
    pub price_usd: Option<f64>,
    /// Where the book's bytes come from.
    pub source: BookSource,
    /// The embedded cover image, when one was found. Absent covers are served as
    /// generated SVG placeholders.
    pub cover: Option<CoverRef>,
}

/// The origin of a book's downloadable content.
#[derive(Debug, Clone)]
pub enum BookSource {
    /// A synthetic sample; its EPUB and cover are generated on demand.
    Sample,
    /// A real EPUB file on disk.
    File { path: PathBuf },
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

    /// Classify a book from its Dublin Core subjects. EPUBs don't carry our
    /// two-way taxonomy, so this is a best-effort heuristic that defaults to
    /// non-fiction when the subjects are unhelpful.
    fn classify(subjects: &[String]) -> Category {
        let joined = subjects.join(" ").to_lowercase();
        if joined.contains("nonfiction") || joined.contains("non-fiction") {
            Category::NonFiction
        } else if joined.contains("fiction") {
            Category::Fiction
        } else {
            Category::NonFiction
        }
    }
}

/// A set of books, indexed by id for lookup.
pub struct Catalog {
    books: Vec<Book>,
    by_id: HashMap<String, usize>,
}

impl Catalog {
    fn from_books(mut books: Vec<Book>) -> Self {
        books.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        let by_id = books
            .iter()
            .enumerate()
            .map(|(i, b)| (b.id.clone(), i))
            .collect();
        Catalog { books, by_id }
    }

    /// All books, ordered by title.
    pub fn books(&self) -> &[Book] {
        &self.books
    }

    /// Look up a single book by its id.
    pub fn get(&self, id: &str) -> Option<&Book> {
        self.by_id.get(id).map(|&i| &self.books[i])
    }

    /// The built-in sample catalog of public-domain titles.
    pub fn sample() -> Self {
        Self::from_books(sample_books())
    }

    /// Scan a directory of `.epub` files and build a catalog from their embedded
    /// metadata. Files that cannot be read or parsed are skipped with a warning,
    /// so a half-written file (mid-copy) simply doesn't appear until it's whole.
    pub fn from_dir(dir: &Path) -> Self {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) => {
                tracing::error!(%err, dir = %dir.display(), "cannot read library directory");
                return Self::from_books(Vec::new());
            }
        };

        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("epub"))
                    .unwrap_or(false)
            })
            .collect();
        paths.sort();

        let mut books = Vec::new();
        let mut used_ids: HashMap<String, u32> = HashMap::new();

        for path in paths {
            let meta = match epub::read_meta(&path) {
                Ok(meta) => meta,
                Err(err) => {
                    tracing::warn!(%err, path = %path.display(), "skipping unreadable EPUB");
                    continue;
                }
            };

            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("book");
            let base = slugify(meta.title.as_deref().unwrap_or(stem));

            // Disambiguate ids that slugify to the same value.
            let seen = used_ids.entry(base.clone()).or_insert(0);
            let id = if *seen == 0 {
                base.clone()
            } else {
                format!("{base}-{}", *seen + 1)
            };
            *seen += 1;

            books.push(Book {
                id,
                title: meta.title.unwrap_or_else(|| stem.to_string()),
                author: meta.author.unwrap_or_else(|| "Unknown Author".to_string()),
                language: meta.language,
                description: meta.description,
                modified: meta.modified,
                category: Category::classify(&meta.subjects),
                price_usd: None,
                source: BookSource::File { path },
                cover: meta.cover,
            });
        }

        tracing::info!(count = books.len(), dir = %dir.display(), "scanned library");
        Self::from_books(books)
    }
}

impl Book {
    /// Build the OPDS `Publication` (as embedded in a feed) for this book.
    pub fn to_publication(&self, base: &str) -> Publication {
        let mut metadata = Metadata::new(self.title.clone());
        metadata.type_ = Some("http://schema.org/Book".to_string());
        metadata.identifier = Some(format!("urn:opds:book:{}", self.id));
        metadata.author = Some(Contributor::new(self.author.clone()));
        metadata.language = self.language.clone();
        metadata.description = self.description.clone();
        metadata.modified = self.modified.clone();

        let self_link = Link::new(format!("{base}/opds/publications/{}", self.id))
            .with_rel("self")
            .with_type(PUBLICATION_MEDIA_TYPE);

        // Acquisition link: a free download, or a paid "buy" link with a price.
        let acquisition = match self.price_usd {
            None => Link::new(format!("{base}/opds/download/{}.epub", self.id))
                .with_rel("http://opds-spec.org/acquisition/open-access")
                .with_type("application/epub+zip"),
            // The buy link points at an HTML purchase page; the actual file is
            // obtained indirectly afterwards, described by indirectAcquisition.
            Some(price) => Link::new(format!("{base}/opds/buy/{}", self.id))
                .with_rel("http://opds-spec.org/acquisition/buy")
                .with_type("text/html")
                .with_properties(LinkProperties {
                    price: Some(Price {
                        currency: "USD".to_string(),
                        value: price,
                    }),
                    indirect_acquisition: vec![IndirectAcquisition {
                        type_: "application/epub+zip".to_string(),
                        child: Vec::new(),
                    }],
                    ..Default::default()
                }),
        };

        // Cover: a real embedded image when we have one, otherwise a generated
        // SVG placeholder (for which we know the exact dimensions).
        let (cover_type, generated) = match &self.cover {
            Some(c) => (c.media_type.clone(), false),
            None => ("image/svg+xml".to_string(), true),
        };
        let mut cover = Link::new(format!("{base}/opds/covers/{}", self.id))
            .with_rel("http://opds-spec.org/image")
            .with_type(cover_type.clone());
        let mut thumbnail = Link::new(format!("{base}/opds/covers/{}/thumb", self.id))
            .with_rel("http://opds-spec.org/image/thumbnail")
            .with_type(cover_type);
        if generated {
            cover = cover.with_dimensions(800, 1200);
            thumbnail = thumbnail.with_dimensions(160, 240);
        }

        Publication {
            metadata,
            links: vec![self_link, acquisition],
            images: vec![cover, thumbnail],
        }
    }
}

/// Turn arbitrary text into a URL-safe, lowercase, hyphenated id.
fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    if out.is_empty() {
        out.push_str("book");
    }
    out
}

/// The built-in sample set, used when no library directory is configured.
fn sample_books() -> Vec<Book> {
    let book = |id: &str,
                title: &str,
                author: &str,
                description: &str,
                modified: &str,
                category: Category,
                price_usd: Option<f64>| Book {
        id: id.to_string(),
        title: title.to_string(),
        author: author.to_string(),
        language: Some("en".to_string()),
        description: Some(description.to_string()),
        modified: Some(modified.to_string()),
        category,
        price_usd,
        source: BookSource::Sample,
        cover: None,
    };

    vec![
        book(
            "moby-dick",
            "Moby-Dick; or, The Whale",
            "Herman Melville",
            "The saga of Captain Ahab and his monomaniacal pursuit of the white whale.",
            "2015-09-29T17:00:00Z",
            Category::Fiction,
            None,
        ),
        book(
            "pride-and-prejudice",
            "Pride and Prejudice",
            "Jane Austen",
            "Elizabeth Bennet navigates manners, upbringing, and marriage.",
            "2016-01-12T09:30:00Z",
            Category::Fiction,
            Some(4.99),
        ),
        book(
            "frankenstein",
            "Frankenstein; or, The Modern Prometheus",
            "Mary Shelley",
            "A scientist creates a sapient creature and reaps the consequences.",
            "2018-07-03T12:00:00Z",
            Category::Fiction,
            None,
        ),
        book(
            "on-the-origin-of-species",
            "On the Origin of Species",
            "Charles Darwin",
            "The foundational work of evolutionary biology.",
            "2017-11-24T00:00:00Z",
            Category::NonFiction,
            Some(2.99),
        ),
        book(
            "the-art-of-war",
            "The Art of War",
            "Sun Tzu",
            "An ancient Chinese treatise on military strategy.",
            "2019-02-14T08:00:00Z",
            Category::NonFiction,
            None,
        ),
    ]
}
