//! The catalog domain types and library scanning.
//!
//! [`Book`] is the server's own representation of a publication (as opposed to
//! the wire `Publication`). A book is a logical work that may be backed by
//! several format files (EPUB, XTC/XTCH) grouped by title + author; books come
//! either from the built-in [`sample_books`] set or from scanning a library
//! directory. Persistence and querying live in [`crate::library`].

use std::borrow::Cow;
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
    pub modified: Option<jiff::Timestamp>,
    /// How the title may be acquired (free download, purchase, or borrow).
    pub acquisition: Acquisition,
    /// Where the book's bytes come from.
    pub source: BookSource,
    /// The embedded cover image, when one was found. Absent covers are served as
    /// generated SVG placeholders.
    pub cover: Option<CoverRef>,
}

/// Where a book's downloadable content comes from.
#[derive(Debug, Clone)]
pub enum BookSource {
    /// A synthetic sample; its EPUB and cover are generated on demand (tests).
    Sample,
    /// One or more real format files on disk, best-metadata format first.
    Files(Vec<BookFile>),
}

/// A single format file backing a book.
#[derive(Debug, Clone)]
pub struct BookFile {
    pub path: PathBuf,
    pub format: Format,
}

/// A supported book file format. Centralizes everything we need to know about a
/// format: its extension, media type, metadata richness, and how to read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Epub,
    Xtc,
    Xtch,
}

impl Format {
    /// The format of `path`, by extension, or `None` if it isn't a supported
    /// book file.
    pub(crate) fn from_path(path: &Path) -> Option<Format> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "epub" => Some(Format::Epub),
            "xtc" => Some(Format::Xtc),
            "xtch" => Some(Format::Xtch),
            _ => None,
        }
    }

    /// The format for a stored media-type string, or `None` if unrecognized.
    pub(crate) fn from_media_type(media_type: &str) -> Option<Format> {
        match media_type {
            "application/epub+zip" => Some(Format::Epub),
            "application/x-xtc" => Some(Format::Xtc),
            "application/x-xtch" => Some(Format::Xtch),
            _ => None,
        }
    }

    /// The media type, used both on the wire and as a download `Content-Type`.
    pub(crate) fn media_type(self) -> &'static str {
        match self {
            Format::Epub => "application/epub+zip",
            Format::Xtc => "application/x-xtc",
            Format::Xtch => "application/x-xtch",
        }
    }

    /// The URL path segment (and file extension) used to request this format.
    pub(crate) fn ext(self) -> &'static str {
        match self {
            Format::Epub => "epub",
            Format::Xtc => "xtc",
            Format::Xtch => "xtch",
        }
    }

    /// Metadata richness: the highest-ranked format present supplies a work's
    /// title/author/cover. EPUB carries the fullest metadata.
    pub(crate) fn rank(self) -> i64 {
        match self {
            Format::Epub => 2,
            Format::Xtc | Format::Xtch => 1,
        }
    }

    /// Read metadata from a file of this format.
    pub(crate) fn read_meta(self, path: &Path) -> std::io::Result<epub::EpubMeta> {
        match self {
            Format::Epub => epub::read_meta(path),
            Format::Xtc | Format::Xtch => crate::xtc::read_meta(path),
        }
    }
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

/// Whether a path names a supported book file.
pub(crate) fn is_book_file(path: &Path) -> bool {
    Format::from_path(path).is_some()
}

/// A stable grouping key for a logical work (its title and author, slugified).
/// Files that produce the same key are treated as formats of the same book.
pub(crate) fn work_key(title: &str, author: &str) -> String {
    format!("{}|{}", slugify(title), slugify(author))
}

/// Recursively collect the supported book files under `root`, sorted. Symlinks
/// are not followed, so symlinked directories can't cause cycles.
pub(crate) fn book_file_paths(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(err) => {
                tracing::warn!(?err, "cannot read directory entry");
                None
            }
        })
        .filter(|entry| entry.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .filter(|path| is_book_file(path))
        .collect();

    out.sort();
    out
}

/// Derive a book's default category when first scanned: prefer a top-level
/// `Fiction`/`Non-Fiction` library subfolder, else classify from the EPUB's
/// Dublin Core subjects (defaulting to non-fiction).
pub(crate) fn derive_category(root: &Path, path: &Path, subjects: &[String]) -> Category {
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

/// Parse an EPUB/RFC 3339 timestamp (e.g. `2015-09-29T17:00:00Z`), ignoring
/// values that aren't a full timestamp (e.g. a bare date).
pub(crate) fn parse_timestamp(s: &str) -> Option<jiff::Timestamp> {
    s.trim().parse().ok()
}

/// Turn arbitrary text into a URL-safe, lowercase, hyphenated slug.
///
/// Non-ASCII letters are transliterated to ASCII first (e.g. `République` ->
/// `republique`, `Œuvres` -> `oeuvres`) so accented titles produce readable
/// slugs instead of dropping the accented characters to hyphens.
pub(crate) fn slugify(input: &str) -> String {
    let ascii = deunicode::deunicode(input);
    let mut out = String::with_capacity(ascii.len());
    let mut pending_dash = false;
    for ch in ascii.chars() {
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

/// A monetary amount in US-dollar cents (minor units), stored and compared as
/// an exact integer instead of a lossy `f64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsdCents(pub u32);

impl UsdCents {
    /// The amount in dollars, for the wire: OPDS `price.value` is a JSON number.
    pub fn as_dollars(self) -> f64 {
        f64::from(self.0) / 100.0
    }
}

/// How a title may be acquired. Exactly one mode applies, so this replaces the
/// old `(price_usd, lendable)` pair and its impossible "priced *and* lendable"
/// combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acquisition {
    /// A free, open-access download.
    OpenAccess,
    /// For sale at the given price.
    Buy(UsdCents),
    /// Available to borrow (library lending).
    Borrow,
}

impl Book {
    /// A freely downloadable title: neither for sale nor lent.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_open_access(&self) -> bool {
        matches!(self.acquisition, Acquisition::OpenAccess)
    }

    /// Build the OPDS `Publication` (as embedded in a feed) for this book.
    pub fn to_publication(&self, base: &str) -> Publication {
        let mut metadata = Metadata::new(self.title.clone());
        metadata.r#type = Some("http://schema.org/Book".into());
        metadata.identifier = Some(format!("urn:opds:book:{}", self.id));
        metadata.author = Some(Contributor::new(self.author.clone()));
        metadata.language = self.language.clone();
        metadata.description = self.description.clone();
        metadata.modified = self.modified.as_ref().map(jiff::Timestamp::to_string);

        let self_link = Link::new(format!("{base}/opds/publications/{}", self.id))
            .with_rel("self")
            .with_type(PUBLICATION_MEDIA_TYPE);

        // Acquisition link: a library borrow, a paid buy, or a free download.
        // Borrow and buy both reach the file indirectly (via an intermediate
        // page), described by indirectAcquisition.
        let epub_indirect = || {
            vec![IndirectAcquisition {
                r#type: "application/epub+zip".into(),
                child: Vec::new(),
            }]
        };
        let mut links = vec![self_link];
        match self.acquisition {
            Acquisition::Borrow => {
                links.push(
                    Link::new(format!("{base}/opds/borrow/{}", self.id))
                        .with_rel("http://opds-spec.org/acquisition/borrow")
                        .with_type("text/html")
                        .with_properties(LinkProperties {
                            indirect_acquisition: epub_indirect(),
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
            }
            Acquisition::Buy(price) => {
                links.push(
                    Link::new(format!("{base}/opds/buy/{}", self.id))
                        .with_rel("http://opds-spec.org/acquisition/buy")
                        .with_type("text/html")
                        .with_properties(LinkProperties {
                            price: Some(Price {
                                currency: "USD".into(),
                                value: price.as_dollars(),
                            }),
                            indirect_acquisition: epub_indirect(),
                            ..Default::default()
                        }),
                );
            }
            Acquisition::OpenAccess => {
                // One download link per available format.
                match &self.source {
                    BookSource::Sample => links.push(
                        Link::new(format!("{base}/opds/download/{}.epub", self.id))
                            .with_rel("http://opds-spec.org/acquisition/open-access")
                            .with_type("application/epub+zip"),
                    ),
                    BookSource::Files(files) => {
                        for file in files {
                            links.push(
                                Link::new(format!(
                                    "{base}/opds/download/{}/{}",
                                    self.id,
                                    file.format.ext()
                                ))
                                .with_rel("http://opds-spec.org/acquisition/open-access")
                                .with_type(file.format.media_type()),
                            );
                        }
                    }
                }
            }
        }

        // Cover: a real embedded image when we have one, otherwise a generated
        // SVG placeholder (for which we know the exact dimensions).
        let (cover_type, generated): (Cow<'static, str>, bool) = match &self.cover {
            Some(c) => (c.media_type.clone().into(), false),
            None => ("image/svg+xml".into(), true),
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
            links,
            images: vec![cover, thumbnail],
        }
    }
}

/// The built-in sample set (with each book's default category). Demo/test
/// scaffolding: a real deployment always serves a library directory.
#[cfg(test)]
pub(crate) fn sample_books() -> Vec<(Book, Category)> {
    let book = |id: &str,
                title: &str,
                author: &str,
                description: &str,
                modified: &str,
                category: Category,
                acquisition: Acquisition|
     -> (Book, Category) {
        let book = Book {
            id: id.to_string(),
            title: title.to_string(),
            author: author.to_string(),
            language: Some("en".to_string()),
            description: Some(description.to_string()),
            modified: parse_timestamp(modified),
            acquisition,
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
            Acquisition::OpenAccess,
        ),
        book(
            "pride-and-prejudice",
            "Pride and Prejudice",
            "Jane Austen",
            "Elizabeth Bennet navigates manners, upbringing, and marriage.",
            "2016-01-12T09:30:00Z",
            fiction(),
            Acquisition::Buy(UsdCents(499)),
        ),
        book(
            "frankenstein",
            "Frankenstein; or, The Modern Prometheus",
            "Mary Shelley",
            "A scientist creates a sapient creature and reaps the consequences.",
            "2018-07-03T12:00:00Z",
            fiction(),
            Acquisition::OpenAccess,
        ),
        book(
            "on-the-origin-of-species",
            "On the Origin of Species",
            "Charles Darwin",
            "The foundational work of evolutionary biology.",
            "2017-11-24T00:00:00Z",
            nonfiction(),
            Acquisition::Buy(UsdCents(299)),
        ),
        book(
            "the-art-of-war",
            "The Art of War",
            "Sun Tzu",
            "An ancient Chinese treatise on military strategy.",
            "2019-02-14T08:00:00Z",
            nonfiction(),
            Acquisition::Borrow, // demonstrates library lending
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn slugify_transliterates_accents() {
        assert_eq!(slugify("Baking at République"), "baking-at-republique");
        assert_eq!(slugify("Naïve Café"), "naive-cafe");
        assert_eq!(slugify("Œuvres complètes"), "oeuvres-completes");
        // Plain ASCII is unchanged; runs of punctuation collapse to one dash.
        assert_eq!(slugify("Moby-Dick; or, The Whale"), "moby-dick-or-the-whale");
        // Empty/blank input still yields a usable slug.
        assert_eq!(slugify(""), "book");
        assert_eq!(slugify("   "), "book");
    }
}
