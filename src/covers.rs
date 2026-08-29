//! Cover images served at runtime: an SVG placeholder for books without an
//! embedded cover, and JPEG thumbnails downscaled from an embedded cover.

use std::io::Cursor;

use crate::catalog::Book;

/// Render an SVG placeholder cover for `book` at the given pixel dimensions,
/// showing the title and author on a solid background.
pub fn cover_svg(book: &Book, width: u32, height: u32) -> String {
    let title = xml_escape(&book.title);
    let author = xml_escape(&book.author);
    // Deterministically pick a background hue from the id so covers differ.
    let hue = book.id.bytes().fold(0u32, |acc, b| acc + b as u32) % 360;
    format!(
        r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">
  <rect width="100%" height="100%" fill="hsl({hue}, 45%, 30%)"/>
  <text x="50%" y="42%" fill="#ffffff" font-family="Georgia, serif" font-size="{title_size}"
        font-weight="bold" text-anchor="middle">{title}</text>
  <text x="50%" y="54%" fill="#dddddd" font-family="Georgia, serif" font-size="{author_size}"
        text-anchor="middle">{author}</text>
</svg>
"##,
        title_size = height / 18,
        author_size = height / 26,
    )
}

/// Downscale a raster cover image to fit within `width`x`height`, preserving
/// aspect ratio, and re-encode it as JPEG. Returns `None` if the bytes can't be
/// decoded (in which case callers serve the original image instead).
pub fn thumbnail(bytes: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let image = image::load_from_memory(bytes).ok()?;
    let thumb = image.thumbnail(width, height);
    let mut out = Cursor::new(Vec::new());
    thumb.write_to(&mut out, image::ImageFormat::Jpeg).ok()?;
    Some(out.into_inner())
}

/// Escape the five XML predefined entities so book metadata can be embedded in
/// XHTML/OPF/SVG documents.
pub(crate) fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}
