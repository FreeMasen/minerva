//! Reading metadata and cover images out of existing EPUB files.
//!
//! This is the counterpart to the EPUB *writer* in [`crate::assets`]. An EPUB is
//! an OCF (zip) container; the entry point is `META-INF/container.xml`, which
//! points at a package document (`.opf`) carrying the Dublin Core metadata and a
//! manifest that (by one of two conventions) identifies the cover image.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, ErrorKind, Read, Seek};
use std::path::Path;

use zip::ZipArchive;

/// A reference to a cover image stored inside an EPUB.
#[derive(Debug, Clone)]
pub struct CoverRef {
    /// Path of the image entry within the zip archive.
    pub zip_path: String,
    pub media_type: String,
}

/// Metadata extracted from an EPUB package document.
#[derive(Debug, Default)]
pub struct EpubMeta {
    pub title: Option<String>,
    pub author: Option<String>,
    pub language: Option<String>,
    pub description: Option<String>,
    pub identifier: Option<String>,
    /// Last-modified timestamp (EPUB3 `dcterms:modified` or `dc:date`).
    pub modified: Option<String>,
    pub subjects: Vec<String>,
    pub cover: Option<CoverRef>,
    /// The series (collection) this book belongs to, if any.
    pub series: Option<String>,
    /// The book's position within its series (e.g. `2` or `1.5`).
    pub series_index: Option<f64>,
}

/// Read and parse the metadata (including cover location) from an EPUB file.
pub fn read_meta(path: &Path) -> io::Result<EpubMeta> {
    let mut zip = ZipArchive::new(File::open(path)?)?;
    let container = read_entry_string(&mut zip, "META-INF/container.xml")?;
    let opf_path = opf_path(&container).ok_or_else(|| {
        io::Error::new(ErrorKind::InvalidData, "container.xml has no rootfile")
    })?;
    let opf = read_entry_string(&mut zip, &opf_path)?;
    parse_opf(&opf, &opf_path).map_err(|e| io::Error::new(ErrorKind::InvalidData, e))
}

/// Read the raw bytes of a single entry (e.g. a cover image) from an EPUB.
pub fn read_entry(path: &Path, name: &str) -> io::Result<Vec<u8>> {
    let mut zip = ZipArchive::new(File::open(path)?)?;
    let mut entry = zip
        .by_name(name)
        .map_err(|e| io::Error::new(ErrorKind::NotFound, format!("{name}: {e}")))?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf)?;
    Ok(buf)
}

fn read_entry_string<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    name: &str,
) -> io::Result<String> {
    let mut entry = zip
        .by_name(name)
        .map_err(|e| io::Error::new(ErrorKind::NotFound, format!("{name}: {e}")))?;
    let mut s = String::new();
    entry.read_to_string(&mut s)?;
    Ok(s)
}

/// Extract the package-document path from `container.xml`.
fn opf_path(container_xml: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(container_xml).ok()?;
    doc.descendants()
        .find(|n| n.tag_name().name() == "rootfile")
        .and_then(|n| n.attribute("full-path"))
        .map(str::to_string)
}

/// An attribute's value by local name, ignoring its XML namespace (so both
/// `file-as` and `opf:file-as` match).
fn attr_local<'a>(n: roxmltree::Node<'a, '_>, local: &str) -> Option<&'a str> {
    n.attributes()
        .find(|a| a.name() == local)
        .map(|a| a.value())
}

/// De-invert a "Last, First" library-sort name into "First Last". Names without
/// a single inverting comma are returned trimmed and unchanged.
fn uninvert_name(name: &str) -> String {
    match name.split_once(',') {
        Some((last, first)) if !first.trim().is_empty() => {
            format!("{} {}", first.trim(), last.trim())
        }
        _ => name.trim().to_string(),
    }
}

fn parse_opf(opf: &str, opf_path: &str) -> Result<EpubMeta, String> {
    let doc = roxmltree::Document::parse(opf).map_err(|e| e.to_string())?;
    let mut meta = EpubMeta::default();

    let text = |n: roxmltree::Node| {
        n.text()
            .map(|t| t.trim().to_string())
            .filter(|s| !s.is_empty())
    };

    // Manifest items, so a cover pointer can be resolved to an href.
    let mut items: HashMap<&str, (&str, &str)> = HashMap::new();
    let mut cover: Option<(String, String)> = None; // (href, media-type)
    let mut cover_meta_id: Option<String> = None; // EPUB2 <meta name="cover" content=..>

    // Series metadata. Calibre stores it as flat <meta name="calibre:series..">;
    // EPUB3 uses a `belongs-to-collection` entry refined by collection-type and
    // group-position, keyed by the collection element's id.
    let mut calibre_series: Option<String> = None;
    let mut calibre_series_index: Option<f64> = None;
    let mut collections: Vec<(Option<String>, String)> = Vec::new(); // (id, name)
    let mut collection_type: HashMap<String, String> = HashMap::new(); // id -> type
    let mut collection_position: HashMap<String, f64> = HashMap::new(); // id -> position

    for n in doc.descendants() {
        match n.tag_name().name() {
            "title" if meta.title.is_none() => meta.title = text(n),
            "creator" if meta.author.is_none() => {
                // Prefer the element text; fall back to the `file-as` attribute
                // (some EPUBs leave the element empty), de-inverting "Last, First".
                meta.author = text(n).or_else(|| attr_local(n, "file-as").map(uninvert_name));
            }
            "language" if meta.language.is_none() => meta.language = text(n),
            "description" if meta.description.is_none() => meta.description = text(n),
            "identifier" if meta.identifier.is_none() => meta.identifier = text(n),
            "subject" => {
                if let Some(s) = text(n) {
                    meta.subjects.push(s);
                }
            }
            // `dc:date` is a fallback; `dcterms:modified` (below) wins.
            "date" if meta.modified.is_none() => meta.modified = text(n),
            "meta" => {
                if n.attribute("property") == Some("dcterms:modified") {
                    if let Some(t) = text(n) {
                        meta.modified = Some(t);
                    }
                }
                if n.attribute("name") == Some("cover") {
                    cover_meta_id = n.attribute("content").map(str::to_string);
                }
                match n.attribute("name") {
                    Some("calibre:series") => {
                        calibre_series = n.attribute("content").map(str::to_string)
                    }
                    Some("calibre:series_index") => {
                        calibre_series_index = n.attribute("content").and_then(|v| v.parse().ok())
                    }
                    _ => {}
                }
                if n.attribute("property") == Some("belongs-to-collection") {
                    if let Some(name) = text(n) {
                        collections.push((n.attribute("id").map(str::to_string), name));
                    }
                }
                if let Some(refines) = n.attribute("refines") {
                    let target = refines.trim_start_matches('#').to_string();
                    match n.attribute("property") {
                        Some("collection-type") => {
                            if let Some(t) = text(n) {
                                collection_type.insert(target, t);
                            }
                        }
                        Some("group-position") => {
                            if let Some(p) = text(n).and_then(|v| v.parse().ok()) {
                                collection_position.insert(target, p);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "item" => {
                if let (Some(id), Some(href)) = (n.attribute("id"), n.attribute("href")) {
                    let mt = n.attribute("media-type").unwrap_or("");
                    items.insert(id, (href, mt));
                    // EPUB3: the cover image carries properties="cover-image".
                    let is_cover = n
                        .attribute("properties")
                        .map(|p| p.split_whitespace().any(|w| w == "cover-image"))
                        .unwrap_or(false);
                    if is_cover {
                        cover = Some((href.to_string(), mt.to_string()));
                    }
                }
            }
            _ => {}
        }
    }

    // EPUB2 fallback: resolve the <meta name="cover"> id against the manifest.
    if cover.is_none() {
        if let Some(id) = cover_meta_id {
            if let Some((href, mt)) = items.get(id.as_str()) {
                cover = Some(((*href).to_string(), (*mt).to_string()));
            }
        }
    }

    // Series: prefer an EPUB3 collection typed "series" (or an untyped one),
    // otherwise fall back to the flat Calibre metadata.
    let epub3_series = collections.iter().find(|(id, _)| {
        id.as_deref()
            .and_then(|i| collection_type.get(i))
            .map(|t| t == "series")
            .unwrap_or(true)
    });
    if let Some((id, name)) = epub3_series {
        meta.series = Some(name.clone());
        meta.series_index = id.as_deref().and_then(|i| collection_position.get(i)).copied();
    } else {
        meta.series = calibre_series;
        meta.series_index = calibre_series_index;
    }

    meta.cover = cover.map(|(href, mt)| {
        let zip_path = resolve_href(opf_path, &href);
        let media_type = if mt.is_empty() {
            guess_media_type(&zip_path)
        } else {
            mt
        };
        CoverRef {
            zip_path,
            media_type,
        }
    });

    Ok(meta)
}

/// Resolve an href from the package document into an absolute zip entry path,
/// honouring the OPF's own directory and any `.`/`..` segments.
fn resolve_href(opf_path: &str, href: &str) -> String {
    let href = percent_decode(href);
    let base = match opf_path.rfind('/') {
        Some(i) => &opf_path[..=i],
        None => "",
    };
    let combined = format!("{base}{href}");

    let mut parts: Vec<&str> = Vec::new();
    for seg in combined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Minimal percent-decoding for hrefs (covers commonly contain `%20`, etc.).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn guess_media_type(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::parse_opf;

    #[test]
    fn parses_calibre_series_and_file_as_author() {
        let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"
            xmlns:opf="http://www.idpf.org/2007/opf">
    <dc:title>Book Two</dc:title>
    <dc:creator opf:file-as="Tolkien, J. R. R."></dc:creator>
    <meta name="calibre:series" content="My Series"/>
    <meta name="calibre:series_index" content="2.5"/>
  </metadata>
</package>"#;
        let meta = parse_opf(opf, "content.opf").unwrap();
        assert_eq!(meta.title.as_deref(), Some("Book Two"));
        // Empty creator element falls back to the de-inverted file-as name.
        assert_eq!(meta.author.as_deref(), Some("J. R. R. Tolkien"));
        assert_eq!(meta.series.as_deref(), Some("My Series"));
        assert_eq!(meta.series_index, Some(2.5));
    }

    #[test]
    fn parses_epub3_belongs_to_collection() {
        let opf = r##"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Book Three</dc:title>
    <dc:creator>An Author</dc:creator>
    <meta property="belongs-to-collection" id="c01">Epic Saga</meta>
    <meta refines="#c01" property="collection-type">series</meta>
    <meta refines="#c01" property="group-position">3</meta>
  </metadata>
</package>"##;
        let meta = parse_opf(opf, "content.opf").unwrap();
        assert_eq!(meta.author.as_deref(), Some("An Author"));
        assert_eq!(meta.series.as_deref(), Some("Epic Saga"));
        assert_eq!(meta.series_index, Some(3.0));
    }
}
