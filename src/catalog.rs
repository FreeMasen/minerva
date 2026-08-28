//! The catalog domain types and EPUB scanning.
//!
//! [`Book`] is the server's own representation of a publication (as opposed to
//! the wire `Publication`). Books come either from the built-in [`sample_books`]
//! set or from scanning a directory of EPUB files; persistence and querying live
//! in [`crate::library`].

use std::path::{Path, PathBuf};

use crate::epub::{self, CoverRef};
use crate::model::*;

/// A single book in the catalog. This is the server's own domain type, kept
/// separate from the wire (`Publication`) representation. A book's categories
/// are a separate many-to-many relation (see [`crate::library`]).
#[derive(Debug, Clone)]
pub struct Book {
    pub id: String,
    pub title: String,
    pub author: String,
    pub language: Option<String>,
    pub description: Option<String>,
    pub modified: Option<String>,
    /// Price in USD for a paid title, or `None` for a free/open-access download.
    pub price_usd: Option<f64>,
    /// Whether the title is borrowed (library lending) rather than downloaded
    /// or bought outright.
    pub lendable: bool,
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

/// A category a book can belong to. Categories are arbitrary and created on
/// demand; a book may belong to several.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Category {
    pub slug: String,
    pub label: String,
}

impl Category {
    pub fn new(slug: impl Into<String>, label: impl Into<String>) -> Self {
        Category {
            slug: slug.into(),
            label: label.into(),
        }
    }
}

/// Whether a path names an EPUB file (by extension).
pub(crate) fn is_epub(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("epub"))
        .unwrap_or(false)
}

/// Recursively collect the `.epub` files under `root`, sorted. Symlinks are not
/// followed, so symlinked directories can't cause cycles.
pub(crate) fn epub_paths(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) => {
                tracing::warn!(?err, dir = %dir.display(), "cannot read directory");
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                stack.push(path);
            } else if is_epub(&path) {
                out.push(path);
            }
        }
    }

    out.sort();
    out
}

/// Build a [`Book`] from a scanned EPUB file and its metadata, along with the
/// default category to file it under. The id is a provisional slug (the store
/// deduplicates on insert).
pub(crate) fn book_from_file(root: &Path, path: PathBuf, meta: epub::EpubMeta) -> (Book, Category) {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("book");
    let category = derive_category(root, &path, &meta.subjects);

    let book = Book {
        id: slugify(meta.title.as_deref().unwrap_or(stem)),
        title: meta.title.unwrap_or_else(|| stem.to_string()),
        author: meta.author.unwrap_or_else(|| "Unknown Author".to_string()),
        language: meta.language,
        description: meta.description,
        modified: meta.modified,
        price_usd: None,
        lendable: false,
        source: BookSource::File { path },
        cover: meta.cover,
    };
    (book, category)
}

/// Derive a book's default category when first scanned: prefer a top-level
/// `Fiction`/`Non-Fiction` library subfolder, else classify from the EPUB's
/// Dublin Core subjects (defaulting to non-fiction).
fn derive_category(root: &Path, path: &Path, subjects: &[String]) -> Category {
    category_from_folder(root, path).unwrap_or_else(|| classify_subjects(subjects))
}

fn category_from_folder(root: &Path, path: &Path) -> Option<Category> {
    let rel = path.strip_prefix(root).ok()?;
    // Require an intervening directory component (dir + filename).
    if rel.components().count() < 2 {
        return None;
    }
    let top = rel.components().next()?.as_os_str().to_str()?.to_lowercase();
    match top.as_str() {
        "fiction" => Some(Category::new("fiction", "Fiction")),
        "non-fiction" | "nonfiction" => Some(Category::new("nonfiction", "Non-Fiction")),
        _ => None,
    }
}

fn classify_subjects(subjects: &[String]) -> Category {
    let joined = subjects.join(" ").to_lowercase();
    const NONFICTION_HINTS: [&str; 7] = [
        "nonfiction",
        "non-fiction",
        "biography",
        "history",
        "science",
        "reference",
        "self-help",
    ];
    if NONFICTION_HINTS.iter().any(|h| joined.contains(h)) {
        Category::new("nonfiction", "Non-Fiction")
    } else if joined.contains("fiction")
        || joined.contains("novel")
        || joined.contains("stories")
    {
        Category::new("fiction", "Fiction")
    } else {
        Category::new("nonfiction", "Non-Fiction")
    }
}

/// Turn arbitrary text into a URL-safe, lowercase, hyphenated slug.
pub(crate) fn slugify(input: &str) -> String {
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

impl Book {
    /// A freely downloadable title: neither for sale nor lent.
    pub fn is_open_access(&self) -> bool {
        self.price_usd.is_none() && !self.lendable
    }

    /// Build the OPDS `Publication` (as embedded in a feed) for this book.
    pub fn to_publication(&self, base: &str) -> Publication {
        let mut metadata = Metadata::new(self.title.clone());
        metadata.r#type = Some("http://schema.org/Book".to_string());
        metadata.identifier = Some(format!("urn:opds:book:{}", self.id));
        metadata.author = Some(Contributor::new(self.author.clone()));
        metadata.language = self.language.clone();
        metadata.description = self.description.clone();
        metadata.modified = self.modified.clone();

        let self_link = Link::new(format!("{base}/opds/publications/{}", self.id))
            .with_rel("self")
            .with_type(PUBLICATION_MEDIA_TYPE);

        // Acquisition link: a library borrow, a paid buy, or a free download.
        // Borrow and buy both reach the file indirectly (via an intermediate
        // page), described by indirectAcquisition.
        let epub_indirect = || {
            vec![IndirectAcquisition {
                r#type: "application/epub+zip".to_string(),
                child: Vec::new(),
            }]
        };
        let acquisition = if self.lendable {
            Link::new(format!("{base}/opds/borrow/{}", self.id))
                .with_rel("http://opds-spec.org/acquisition/borrow")
                .with_type("text/html")
                .with_properties(LinkProperties {
                    indirect_acquisition: epub_indirect(),
                    availability: Some(Availability {
                        state: "available".to_string(),
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
                })
        } else if let Some(price) = self.price_usd {
            Link::new(format!("{base}/opds/buy/{}", self.id))
                .with_rel("http://opds-spec.org/acquisition/buy")
                .with_type("text/html")
                .with_properties(LinkProperties {
                    price: Some(Price {
                        currency: "USD".to_string(),
                        value: price,
                    }),
                    indirect_acquisition: epub_indirect(),
                    ..Default::default()
                })
        } else {
            Link::new(format!("{base}/opds/download/{}.epub", self.id))
                .with_rel("http://opds-spec.org/acquisition/open-access")
                .with_type("application/epub+zip")
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

/// The built-in sample set (with each book's default category), used when no
/// library directory is configured.
pub(crate) fn sample_books() -> Vec<(Book, Category)> {
    let book = |id: &str,
                title: &str,
                author: &str,
                description: &str,
                modified: &str,
                category: Category,
                price_usd: Option<f64>,
                lendable: bool|
     -> (Book, Category) {
        let book = Book {
            id: id.to_string(),
            title: title.to_string(),
            author: author.to_string(),
            language: Some("en".to_string()),
            description: Some(description.to_string()),
            modified: Some(modified.to_string()),
            price_usd,
            lendable,
            source: BookSource::Sample,
            cover: None,
        };
        (book, category)
    };

    let fiction = || Category::new("fiction", "Fiction");
    let nonfiction = || Category::new("nonfiction", "Non-Fiction");

    vec![
        book(
            "moby-dick",
            "Moby-Dick; or, The Whale",
            "Herman Melville",
            "The saga of Captain Ahab and his monomaniacal pursuit of the white whale.",
            "2015-09-29T17:00:00Z",
            fiction(),
            None,
            false,
        ),
        book(
            "pride-and-prejudice",
            "Pride and Prejudice",
            "Jane Austen",
            "Elizabeth Bennet navigates manners, upbringing, and marriage.",
            "2016-01-12T09:30:00Z",
            fiction(),
            Some(4.99),
            false,
        ),
        book(
            "frankenstein",
            "Frankenstein; or, The Modern Prometheus",
            "Mary Shelley",
            "A scientist creates a sapient creature and reaps the consequences.",
            "2018-07-03T12:00:00Z",
            fiction(),
            None,
            false,
        ),
        book(
            "on-the-origin-of-species",
            "On the Origin of Species",
            "Charles Darwin",
            "The foundational work of evolutionary biology.",
            "2017-11-24T00:00:00Z",
            nonfiction(),
            Some(2.99),
            false,
        ),
        book(
            "the-art-of-war",
            "The Art of War",
            "Sun Tzu",
            "An ancient Chinese treatise on military strategy.",
            "2019-02-14T08:00:00Z",
            nonfiction(),
            None,
            true, // borrowable — demonstrates library lending
        ),
    ]
}
