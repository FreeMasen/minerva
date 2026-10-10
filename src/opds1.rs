//! The OPDS 1.2 catalog, served under `/opds1`.
//!
//! A parallel presentation of the same catalog the OPDS 2.0 handlers in
//! [`crate::main`] serve: the store, the domain types and the asset endpoints
//! (downloads and covers, under `/opds`) are shared, and only the feed
//! structure and wire format differ.
//!
//! The two versions live at separate URLs rather than negotiating on `Accept`.
//! OPDS 1.x clients commonly send `Accept: */*`, and every href inside an Atom
//! feed has to resolve to Atom for browsing to continue — so a feed's version
//! is settled by its path, not by a header the client may not send.
//!
//! OPDS 1.x has no equivalent of a 2.0 `group`, so the category and author
//! browse groups the 2.0 root feed inlines become navigation feeds of their
//! own at `/opds1/categories` and `/opds1/authors`.

use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
    routing::get,
};

use crate::atom::{self, FeedKind};
use crate::catalog::Book;
use crate::model::{AUTH_MEDIA_TYPE, FEED_MEDIA_TYPE, LinkProperties, rel};
use crate::{AppState, PageParams, PageWindow, SearchParams, not_found, page_window};

/// The OPDS 1.x routes (merged into the protected router, so authentication
/// covers both catalog versions identically).
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/opds1", get(root))
        .route("/opds1/new", get(new_publications))
        .route("/opds1/all", get(all_publications))
        .route("/opds1/categories", get(category_index))
        .route("/opds1/category/{slug}", get(category_feed))
        .route("/opds1/authors", get(author_index))
        .route("/opds1/authors/{slug}", get(author_feed))
        .route("/opds1/series", get(series_index))
        .route("/opds1/series/{slug}", get(series_feed))
        .route("/opds1/publications/{id}", get(publication))
        .route("/opds1/search", get(search))
        .route("/opds1/opensearch.xml", get(opensearch))
}

/// An XML response carrying an OPDS 1.x media type (the 1.x sibling of
/// [`crate::Opds`]).
struct Atom {
    body: Vec<u8>,
    media_type: &'static str,
}

impl Atom {
    fn feed(feed: atom::Feed) -> Self {
        Atom {
            media_type: feed.kind.media_type(),
            body: feed.to_xml(),
        }
    }

    fn entry(entry: &atom::Entry) -> Self {
        Atom {
            body: entry.to_xml(),
            media_type: atom::ENTRY_MEDIA_TYPE,
        }
    }

    fn opensearch(description: &atom::OpenSearchDescription) -> Self {
        Atom {
            body: description.to_xml(),
            media_type: atom::OPENSEARCH_MEDIA_TYPE,
        }
    }
}

impl IntoResponse for Atom {
    fn into_response(self) -> Response {
        ([(header::CONTENT_TYPE, self.media_type)], self.body).into_response()
    }
}

// --- Shared links ---

/// The `start` link back to the catalog root, carried by every feed.
fn start_link(base: &str) -> atom::Link {
    atom::Link::new(
        format!("{base}/opds1"),
        "start",
        atom::NAVIGATION_MEDIA_TYPE,
    )
}

/// An `up` link to `{base}/opds1{path}`, a navigation feed.
fn up_link(base: &str, path: &str) -> atom::Link {
    atom::Link::new(
        format!("{base}/opds1{path}"),
        "up",
        atom::NAVIGATION_MEDIA_TYPE,
    )
}

/// The `search` link. OPDS 1.x discovers search through an OpenSearch
/// description document rather than 2.0's templated link.
fn search_link(base: &str) -> atom::Link {
    atom::Link::new(
        format!("{base}/opds1/opensearch.xml"),
        "search",
        atom::OPENSEARCH_MEDIA_TYPE,
    )
    .with_title("Search the catalog")
}

// --- Entry builders ---

/// The Atom entry for a book, as it appears inside an acquisition feed.
///
/// The acquisition and cover hrefs point into the shared `/opds` tree: those
/// endpoints serve bytes, not feeds, so there is nothing version-specific
/// about them.
fn entry_for(book: &Book, base: &str) -> atom::Entry {
    let mut entry = atom::Entry::new(format!("urn:opds:book:{}", book.id), book.title.clone());
    // Atom requires `updated`; a book with no recorded file mtime falls back
    // to the time the feed was built.
    entry.updated = book.modified.unwrap_or_else(jiff::Timestamp::now);
    entry.author = Some(book.author.clone());
    entry.language = book.language.clone();
    entry.summary = book.description.clone();
    entry.series = book
        .series
        .clone()
        .map(|name| (name, book.series_index));

    // Where a client goes for this publication's full detail.
    entry.links.push(atom::Link::new(
        format!("{base}/opds1/publications/{}", book.id),
        "alternate",
        atom::ENTRY_MEDIA_TYPE,
    ));

    match book.indirect_acquisition() {
        // A borrow or a purchase: one link to the intermediate page, carrying
        // the price or the lending availability.
        Some((relation, segment)) => {
            let mut link = atom::Link::new(
                format!("{base}/opds/{segment}/{}", book.id),
                relation,
                "text/html",
            );
            if let Some(properties) = book.acquisition_properties() {
                link = link.with_properties(properties);
            }
            entry.links.push(link);
        }
        // A free title: one download link per available format. EPUB comes
        // first, which matters because clients pick a format by media type and
        // ignore the ones they don't know (XTC/XTCH among them).
        None => {
            for (path, media_type) in book.download_paths() {
                entry.links.push(atom::Link::new(
                    format!("{base}{path}"),
                    rel::OPEN_ACCESS,
                    media_type,
                ));
            }
        }
    }

    let cover_type = book.cover_media_type();
    entry.links.push(atom::Link::new(
        format!("{base}/opds/covers/{}", book.id),
        rel::IMAGE,
        cover_type.clone(),
    ));
    entry.links.push(atom::Link::new(
        format!("{base}/opds/covers/{}/thumb", book.id),
        rel::THUMBNAIL,
        cover_type,
    ));
    entry
}

/// A navigation entry pointing at a further feed, with a description clients
/// render under the title.
fn nav_entry(href: String, title: &str, summary: &str, kind: FeedKind) -> atom::Entry {
    let mut entry = atom::Entry::new(href.clone(), title);
    entry.summary = Some(summary.to_string());
    entry.with_link(atom::Link::new(href, "subsection", kind.media_type()))
}

/// A navigation entry for a browsable subset of the catalog, carrying its size
/// both as prose (which clients display) and as `thr:count` (which they read).
fn count_entry(href: String, title: &str, count: u64) -> atom::Entry {
    let mut entry = atom::Entry::new(href.clone(), title);
    let unit = if count == 1 { "publication" } else { "publications" };
    entry.summary = Some(format!("{count} {unit}"));
    entry.with_link(
        atom::Link::new(href, "subsection", atom::ACQUISITION_MEDIA_TYPE).with_properties(
            LinkProperties {
                number_of_items: Some(count),
                ..Default::default()
            },
        ),
    )
}

/// The OpenSearch counts for a feed that is complete rather than paginated.
/// `items_per_page` is held at a minimum of 1 so a client computing a page
/// count from it cannot divide by zero on an empty feed.
fn complete_counts(total: u64) -> atom::Counts {
    atom::Counts {
        total_results: total,
        items_per_page: total.max(1),
        start_index: 1,
    }
}

// --- Handlers ---

/// The catalog root: a navigation feed whose entries are the browsable views.
async fn root(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let base = &state.base_url;

    let mut feed = atom::Feed::new(
        FeedKind::Navigation,
        format!("{base}/opds1"),
        "Example OPDS Catalog",
    )
    .with_subtitle("A sample catalog demonstrating OPDS 1.2 over Axum.")
    .with_link(start_link(base))
    .with_link(search_link(base))
    .with_link(
        atom::Link::new(
            format!("{base}/opds1/new"),
            rel::SORT_NEW,
            atom::ACQUISITION_MEDIA_TYPE,
        )
        .with_title("New Publications"),
    )
    .with_link(
        // The same catalog as OPDS 2.0, for clients that speak it.
        atom::Link::new(format!("{base}/opds"), "alternate", FEED_MEDIA_TYPE)
            .with_title("OPDS 2.0 catalog"),
    );

    if state.auth.is_some() {
        feed = feed.with_link(atom::Link::new(
            format!("{base}/opds/auth"),
            rel::AUTH_DOCUMENT,
            AUTH_MEDIA_TYPE,
        ));
    }

    feed.entries.push(nav_entry(
        format!("{base}/opds1/all"),
        "All Publications",
        "Every title in the catalog.",
        FeedKind::Acquisition,
    ));
    feed.entries.push(nav_entry(
        format!("{base}/opds1/new"),
        "New Publications",
        "The most recently added titles.",
        FeedKind::Acquisition,
    ));
    // Each browse feed is advertised only when it has something to show.
    if !state.catalog.categories().await.is_empty() {
        feed.entries.push(nav_entry(
            format!("{base}/opds1/categories"),
            "Browse by Category",
            "Titles grouped by category.",
            FeedKind::Navigation,
        ));
    }
    if !state.catalog.authors().await.is_empty() {
        feed.entries.push(nav_entry(
            format!("{base}/opds1/authors"),
            "Browse by Author",
            "Titles grouped by author.",
            FeedKind::Navigation,
        ));
    }
    if !state.catalog.series().await.is_empty() {
        feed.entries.push(nav_entry(
            format!("{base}/opds1/series"),
            "Browse by Series",
            "Titles grouped by series, in reading order.",
            FeedKind::Navigation,
        ));
    }

    Atom::feed(feed)
}

/// An acquisition feed of the most recently added titles.
async fn new_publications(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let base = &state.base_url;
    let books = state.catalog.recent(state.page_size).await;

    let mut feed = atom::Feed::new(
        FeedKind::Acquisition,
        format!("{base}/opds1/new"),
        "New Publications",
    )
    .with_link(start_link(base))
    .with_link(up_link(base, ""));

    feed.counts = Some(complete_counts(books.len() as u64));
    feed.entries = books.iter().map(|book| entry_for(book, base)).collect();
    Atom::feed(feed)
}

/// An acquisition feed of every publication: paginated with RFC 5005 links,
/// counted with OpenSearch metadata, and faceted by category.
async fn all_publications(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PageParams>,
) -> impl IntoResponse {
    let base = &state.base_url;
    let page_size = state.page_size;
    let total = state.catalog.count().await;
    let PageWindow {
        page,
        last_page,
        offset,
    } = page_window(total, page_size, params.page);
    let books = state.catalog.page(page_size, offset).await;

    let page_href = |p: u64| format!("{base}/opds1/all?page={p}");
    let page_link = |p: u64, relation: &'static str| {
        atom::Link::new(page_href(p), relation, atom::ACQUISITION_MEDIA_TYPE)
    };

    let mut feed = atom::Feed::new(
        FeedKind::Acquisition,
        page_href(page),
        "All Publications",
    )
    .with_link(start_link(base))
    .with_link(up_link(base, ""))
    .with_link(search_link(base))
    .with_link(page_link(1, "first"))
    .with_link(page_link(last_page, "last"));

    if page > 1 {
        feed = feed.with_link(page_link(page - 1, "previous"));
    }
    if page < last_page {
        feed = feed.with_link(page_link(page + 1, "next"));
    }

    feed.counts = Some(atom::Counts {
        total_results: total,
        items_per_page: page_size,
        // OpenSearch counts from 1.
        start_index: offset + 1,
    });
    feed.entries = books.iter().map(|book| entry_for(book, base)).collect();

    // Facets: the same category breakdown the 2.0 feed nests in `facets`,
    // flattened onto the feed's own links and grouped by attribute.
    for (category, count) in state.catalog.categories().await {
        feed.links.push(
            atom::Link::new(
                format!("{base}/opds1/category/{}", category.slug),
                rel::FACET,
                atom::ACQUISITION_MEDIA_TYPE,
            )
            .with_title(category.label)
            .with_facet("Category", false)
            .with_properties(LinkProperties {
                number_of_items: Some(count),
                ..Default::default()
            }),
        );
    }

    Atom::feed(feed)
}

/// A navigation feed listing every category.
async fn category_index(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let base = &state.base_url;
    let mut feed = atom::Feed::new(
        FeedKind::Navigation,
        format!("{base}/opds1/categories"),
        "Browse by Category",
    )
    .with_link(start_link(base))
    .with_link(up_link(base, ""));

    feed.entries = state
        .catalog
        .categories()
        .await
        .into_iter()
        .map(|(category, count)| {
            count_entry(
                format!("{base}/opds1/category/{}", category.slug),
                &category.label,
                count,
            )
        })
        .collect();

    Atom::feed(feed)
}

/// An acquisition feed filtered to a single category.
async fn category_feed(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
) -> Response {
    let base = &state.base_url;

    let Some(category) = state.catalog.category(&slug).await else {
        return not_found("No such category");
    };
    let books = state.catalog.books_in_category(&slug).await;

    let mut feed = atom::Feed::new(
        FeedKind::Acquisition,
        format!("{base}/opds1/category/{slug}"),
        category.label,
    )
    .with_link(start_link(base))
    .with_link(up_link(base, "/categories"));

    feed.counts = Some(complete_counts(books.len() as u64));
    feed.entries = books.iter().map(|book| entry_for(book, base)).collect();
    Atom::feed(feed).into_response()
}

/// A navigation feed listing every author.
async fn author_index(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let base = &state.base_url;
    let mut feed = atom::Feed::new(
        FeedKind::Navigation,
        format!("{base}/opds1/authors"),
        "Browse by Author",
    )
    .with_link(start_link(base))
    .with_link(up_link(base, ""));

    feed.entries = state
        .catalog
        .authors()
        .await
        .into_iter()
        .map(|(author, count)| {
            count_entry(
                format!("{base}/opds1/authors/{}", author.slug),
                &author.label,
                count,
            )
        })
        .collect();

    Atom::feed(feed)
}

/// A navigation feed listing every series.
async fn series_index(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let base = &state.base_url;
    let mut feed = atom::Feed::new(
        FeedKind::Navigation,
        format!("{base}/opds1/series"),
        "Browse by Series",
    )
    .with_link(start_link(base))
    .with_link(up_link(base, ""));

    feed.entries = state
        .catalog
        .series()
        .await
        .into_iter()
        .map(|(series, count)| {
            count_entry(
                format!("{base}/opds1/series/{}", series.slug),
                &series.label,
                count,
            )
        })
        .collect();

    Atom::feed(feed)
}

/// An acquisition feed of one series, in reading order.
async fn series_feed(State(state): State<Arc<AppState>>, Path(slug): Path<String>) -> Response {
    let base = &state.base_url;

    let Some(series) = state.catalog.series_by_slug(&slug).await else {
        return not_found("No such series");
    };
    let books = state.catalog.books_in_series(&series).await;

    let mut feed = atom::Feed::new(
        FeedKind::Acquisition,
        format!("{base}/opds1/series/{slug}"),
        series,
    )
    .with_link(start_link(base))
    .with_link(up_link(base, "/series"));

    feed.counts = Some(complete_counts(books.len() as u64));
    feed.entries = books.iter().map(|book| entry_for(book, base)).collect();
    Atom::feed(feed).into_response()
}

/// An acquisition feed of everything by a single author.
async fn author_feed(State(state): State<Arc<AppState>>, Path(slug): Path<String>) -> Response {
    let base = &state.base_url;

    let Some(author) = state.catalog.author_by_slug(&slug).await else {
        return not_found("No such author");
    };
    let books = state.catalog.books_by_author(&author).await;

    let mut feed = atom::Feed::new(
        FeedKind::Acquisition,
        format!("{base}/opds1/authors/{slug}"),
        author,
    )
    .with_link(start_link(base))
    .with_link(up_link(base, "/authors"));

    feed.counts = Some(complete_counts(books.len() as u64));
    feed.entries = books.iter().map(|book| entry_for(book, base)).collect();
    Atom::feed(feed).into_response()
}

/// A single publication as a "complete entry" document: the feed entry plus
/// the full description and the publication's categories.
async fn publication(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let base = &state.base_url;

    let Some(book) = state.catalog.get(&id).await else {
        return not_found("No such publication");
    };

    let mut entry = entry_for(&book, base);
    // The description is free-form text from the book's metadata, which may
    // carry markup; Atom's `html` content type is its correct carrier.
    entry.content = book.description.clone();
    entry.categories = state
        .catalog
        .book_categories(&id)
        .await
        .into_iter()
        .map(|category| atom::Category {
            term: category.slug.to_string(),
            label: category.label,
        })
        .collect();

    // In its own document the publication names itself with `self`, rather
    // than pointing at itself with the feed form's `alternate`.
    entry.links.retain(|link| link.rel.as_ref() != "alternate");
    entry.links.insert(
        0,
        atom::Link::new(
            format!("{base}/opds1/publications/{}", book.id),
            "self",
            atom::ENTRY_MEDIA_TYPE,
        ),
    );

    Atom::entry(&entry).into_response()
}

/// A search feed. The terms are interpreted exactly as the 2.0 search endpoint
/// interprets them.
async fn search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    let base = &state.base_url;

    let (query, author, title) = params.terms();
    let matches = state.catalog.search(&query, &author, &title).await;

    let mut feed = atom::Feed::new(
        FeedKind::Acquisition,
        format!("{base}/opds1/search?{}", params.query_string()),
        "Search results",
    )
    .with_link(start_link(base))
    .with_link(up_link(base, ""))
    .with_link(search_link(base));

    feed.counts = Some(complete_counts(matches.len() as u64));
    feed.entries = matches.iter().map(|book| entry_for(book, base)).collect();
    Atom::feed(feed)
}

/// The OpenSearch description document describing the search endpoint.
async fn opensearch(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let base = &state.base_url;
    Atom::opensearch(&atom::OpenSearchDescription {
        short_name: "Minerva".to_string(),
        description: "Search the catalog".to_string(),
        template: format!("{base}/opds1/search?query={{searchTerms}}"),
    })
}
