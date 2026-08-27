//! An OPDS 2.0 catalog server built on Axum.
//!
//! Implements the core of the OPDS 2.0 specification
//! (<https://specs.opds.io/opds-2.0.html>): a root navigation feed, acquisition
//! feeds (all publications and per-category), facets, full-text search via a
//! templated link, and individual publication documents.

mod assets;
mod catalog;
mod model;

use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;

use crate::catalog::{Book, Category};
use crate::model::*;

/// Shared application configuration.
#[derive(Clone)]
struct AppState {
    /// The externally-visible base URL used to build absolute hrefs.
    base_url: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "opds_axum=debug,tower_http=debug,info".into()),
        )
        .init();

    let base_url =
        std::env::var("OPDS_BASE_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
    let state = Arc::new(AppState { base_url });

    let addr = "0.0.0.0:3000";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    tracing::info!("OPDS server listening on http://{addr}/opds");
    axum::serve(listener, app(state)).await.unwrap();
}

/// Build the application router. Kept separate from `main` so tests can drive
/// the fully-wired app without binding a socket.
fn app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(root_redirect))
        .route("/opds", get(root_feed))
        .route("/opds/all", get(all_publications))
        .route("/opds/category/{slug}", get(category_feed))
        .route("/opds/publications/{id}", get(publication))
        .route("/opds/search", get(search))
        .route("/opds/download/{file}", get(download))
        .route("/opds/buy/{id}", get(buy))
        .route("/opds/covers/{file}", get(cover))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

/// Number of publications served per page in acquisition feeds.
const PAGE_SIZE: u64 = 3;

/// A response that serializes a value to JSON with an OPDS media type.
struct Opds<T: serde::Serialize> {
    value: T,
    media_type: &'static str,
}

impl<T: serde::Serialize> Opds<T> {
    fn feed(value: T) -> Self {
        Opds {
            value,
            media_type: FEED_MEDIA_TYPE,
        }
    }

    fn publication(value: T) -> Self {
        Opds {
            value,
            media_type: PUBLICATION_MEDIA_TYPE,
        }
    }
}

impl<T: serde::Serialize> IntoResponse for Opds<T> {
    fn into_response(self) -> Response {
        match serde_json::to_vec(&self.value) {
            Ok(body) => ([(header::CONTENT_TYPE, self.media_type)], body).into_response(),
            Err(err) => {
                tracing::error!(%err, "failed to serialize OPDS document");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

/// Send visitors at `/` to the catalog root.
async fn root_redirect() -> Response {
    axum::response::Redirect::temporary("/opds").into_response()
}

/// The root navigation feed: entry point that links to the browsable sections.
async fn root_feed(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let base = &state.base_url;

    let mut feed = Feed::new("Example OPDS Catalog", format!("{base}/opds"))
        .with_link(
            Link::new(format!("{base}/opds"))
                .with_rel("start")
                .with_type(FEED_MEDIA_TYPE),
        )
        .with_link(
            // A templated search link, per the OPDS search convention.
            Link::new(format!("{base}/opds/search{{?query}}"))
                .with_rel("search")
                .with_type(FEED_MEDIA_TYPE)
                .with_title("Search the catalog")
                .templated(),
        );

    feed.metadata.description =
        Some("A sample catalog demonstrating OPDS 2.0 over Axum.".to_string());

    feed.navigation = vec![
        Link::new(format!("{base}/opds/all"))
            .with_rel("http://opds-spec.org/sort/new")
            .with_type(FEED_MEDIA_TYPE)
            .with_title("All Publications"),
        Link::new(format!("{base}/opds/category/fiction"))
            .with_rel("subsection")
            .with_type(FEED_MEDIA_TYPE)
            .with_title("Fiction"),
        Link::new(format!("{base}/opds/category/nonfiction"))
            .with_rel("subsection")
            .with_type(FEED_MEDIA_TYPE)
            .with_title("Non-Fiction"),
    ];

    Opds::feed(feed)
}

#[derive(Debug, Deserialize)]
struct PageParams {
    page: Option<u64>,
}

/// An acquisition feed containing every publication, paginated, with category
/// facets and `first`/`previous`/`next`/`last` pagination links.
async fn all_publications(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PageParams>,
) -> impl IntoResponse {
    let base = &state.base_url;
    let books = catalog::books();
    let total = books.len() as u64;

    // At least one page even when the catalog is empty.
    let last_page = total.div_ceil(PAGE_SIZE).max(1);
    let page = params.page.unwrap_or(1).clamp(1, last_page);

    let start = ((page - 1) * PAGE_SIZE) as usize;
    let end = (start + PAGE_SIZE as usize).min(books.len());
    let page_books = &books[start..end];

    let page_href = |p: u64| format!("{base}/opds/all?page={p}");

    let mut feed = Feed::new("All Publications", page_href(page))
        .with_link(
            Link::new(format!("{base}/opds"))
                .with_rel("start")
                .with_type(FEED_MEDIA_TYPE),
        )
        .with_link(
            Link::new(page_href(1))
                .with_rel("first")
                .with_type(FEED_MEDIA_TYPE),
        )
        .with_link(
            Link::new(page_href(last_page))
                .with_rel("last")
                .with_type(FEED_MEDIA_TYPE),
        );

    if page > 1 {
        feed = feed.with_link(
            Link::new(page_href(page - 1))
                .with_rel("previous")
                .with_type(FEED_MEDIA_TYPE),
        );
    }
    if page < last_page {
        feed = feed.with_link(
            Link::new(page_href(page + 1))
                .with_rel("next")
                .with_type(FEED_MEDIA_TYPE),
        );
    }

    feed.metadata.number_of_items = Some(total);
    feed.metadata.items_per_page = Some(PAGE_SIZE);
    feed.metadata.current_page = Some(page);
    feed.publications = page_books.iter().map(|b| b.to_publication(base)).collect();
    feed.facets = vec![category_facet(base, &books)];

    Opds::feed(feed)
}

/// An acquisition feed filtered to a single category.
async fn category_feed(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
) -> Response {
    let base = &state.base_url;

    let Some(category) = Category::from_slug(&slug) else {
        return not_found("No such category");
    };

    let books: Vec<Book> = catalog::books()
        .into_iter()
        .filter(|b| b.category == category)
        .collect();

    let mut feed = Feed::new(
        category.label(),
        format!("{base}/opds/category/{slug}"),
    )
    .with_link(
        Link::new(format!("{base}/opds"))
            .with_rel("start")
            .with_type(FEED_MEDIA_TYPE),
    )
    .with_link(
        Link::new(format!("{base}/opds/all"))
            .with_rel("up")
            .with_type(FEED_MEDIA_TYPE),
    );

    feed.metadata.number_of_items = Some(books.len() as u64);
    feed.publications = books.iter().map(|b| b.to_publication(base)).collect();

    Opds::feed(feed).into_response()
}

/// A single publication document.
async fn publication(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match catalog::book(&id) {
        Some(book) => Opds::publication(book.to_publication(&state.base_url)).into_response(),
        None => not_found("No such publication"),
    }
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    #[serde(default)]
    query: String,
}

/// A search feed: returns publications whose title, author or description match.
async fn search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    let base = &state.base_url;
    let needle = params.query.trim().to_lowercase();

    let matches: Vec<Book> = if needle.is_empty() {
        Vec::new()
    } else {
        catalog::books()
            .into_iter()
            .filter(|b| {
                b.title.to_lowercase().contains(&needle)
                    || b.author.to_lowercase().contains(&needle)
                    || b.description.to_lowercase().contains(&needle)
            })
            .collect()
    };

    let self_href = format!(
        "{base}/opds/search?query={}",
        urlencode(&params.query)
    );

    let mut feed = Feed::new(
        format!("Search results for \"{}\"", params.query),
        self_href,
    )
    .with_link(
        Link::new(format!("{base}/opds"))
            .with_rel("start")
            .with_type(FEED_MEDIA_TYPE),
    );

    feed.metadata.number_of_items = Some(matches.len() as u64);
    feed.publications = matches.iter().map(|b| b.to_publication(base)).collect();

    Opds::feed(feed)
}

/// Serve the open-access EPUB for a publication. The path carries the filename
/// (`{id}.epub`); we strip the extension to recover the book id.
async fn download(Path(file): Path<String>) -> Response {
    let id = file.strip_suffix(".epub").unwrap_or(&file);
    match catalog::book(id) {
        Some(book) if book.price_usd.is_none() => {
            let bytes = assets::epub_bytes(&book);
            (
                [
                    (header::CONTENT_TYPE, "application/epub+zip".to_string()),
                    (
                        header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{id}.epub\""),
                    ),
                ],
                bytes,
            )
                .into_response()
        }
        // A paid title is not available as a free download.
        Some(_) => not_found("This publication is not available for open-access download"),
        None => not_found("No such publication"),
    }
}

/// A minimal "buy" landing page for a paid publication. A real store would
/// begin a purchase flow here; this returns a human-readable placeholder.
async fn buy(Path(id): Path<String>) -> Response {
    match catalog::book(&id) {
        Some(book) => {
            let price = book
                .price_usd
                .map(|p| format!("${p:.2} USD"))
                .unwrap_or_else(|| "free".to_string());
            let body = format!(
                "<!doctype html><html><head><meta charset=\"utf-8\">\
                 <title>Buy {title}</title></head><body>\
                 <h1>{title}</h1><p>by {author}</p>\
                 <p>Price: {price}</p>\
                 <p>This is a placeholder purchase page generated by opds-axum.</p>\
                 </body></html>",
                title = book.title,
                author = book.author,
            );
            ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response()
        }
        None => not_found("No such publication"),
    }
}

/// Serve a generated SVG cover. The filename is `{id}.svg` for the full cover
/// or `{id}-thumb.svg` for the thumbnail.
async fn cover(Path(file): Path<String>) -> Response {
    let stem = file.strip_suffix(".svg").unwrap_or(&file);
    let (id, width, height) = match stem.strip_suffix("-thumb") {
        Some(id) => (id, 160, 240),
        None => (stem, 800, 1200),
    };

    match catalog::book(id) {
        Some(book) => {
            let svg = assets::cover_svg(&book, width, height);
            (
                [(header::CONTENT_TYPE, "image/svg+xml")],
                svg,
            )
                .into_response()
        }
        None => not_found("No such cover"),
    }
}

/// Build a "Category" facet group linking to each category feed.
fn category_facet(base: &str, books: &[Book]) -> Facet {
    let count = |category: Category| {
        books.iter().filter(|b| b.category == category).count() as u64
    };

    let facet_link = |category: Category| {
        Link::new(format!("{base}/opds/category/{}", category.slug()))
            .with_type(FEED_MEDIA_TYPE)
            .with_title(category.label())
            .with_properties(LinkProperties {
                number_of_items: Some(count(category)),
                ..Default::default()
            })
    };

    Facet {
        metadata: Metadata::new("Category"),
        links: vec![
            facet_link(Category::Fiction),
            facet_link(Category::NonFiction),
        ],
    }
}

/// A minimal application/x-www-form-urlencoded-style encoder for query values.
fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// A plain-text 404 response.
fn not_found(message: &str) -> Response {
    (StatusCode::NOT_FOUND, message.to_string()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt; // for `oneshot`

    const BASE: &str = "http://localhost:3000";

    fn test_app() -> Router {
        app(Arc::new(AppState {
            base_url: BASE.to_string(),
        }))
    }

    /// Issue a GET and return the status, content-type, and parsed JSON body
    /// (`Value::Null` if the body is not JSON).
    async fn get(uri: &str) -> (StatusCode, String, Value) {
        let response = test_app()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();

        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, content_type, json)
    }

    /// Issue a GET and return the status, content-type, all response headers,
    /// and the raw body bytes.
    async fn get_raw(uri: &str) -> (StatusCode, String, axum::http::HeaderMap, Vec<u8>) {
        let response = test_app()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();

        let status = response.status();
        let headers = response.headers().clone();
        let content_type = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, content_type, headers, bytes)
    }

    /// Collect the `rel` values present across a JSON array of links.
    fn rels(links: &Value) -> Vec<String> {
        links
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|l| l["rel"].as_str().map(str::to_string))
            .collect()
    }

    #[tokio::test]
    async fn root_redirects_to_opds() {
        let response = test_app()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(response.headers()[header::LOCATION], "/opds");
    }

    #[tokio::test]
    async fn root_feed_has_navigation_and_templated_search() {
        let (status, content_type, json) = get("/opds").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type, FEED_MEDIA_TYPE);
        assert_eq!(json["metadata"]["title"], "Example OPDS Catalog");
        assert!(!json["navigation"].as_array().unwrap().is_empty());

        let search = json["links"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["rel"] == "search")
            .expect("a search link");
        assert_eq!(search["templated"], true);
        assert_eq!(search["href"], format!("{BASE}/opds/search{{?query}}"));
    }

    #[tokio::test]
    async fn all_publications_first_page_paginates() {
        let (status, content_type, json) = get("/opds/all?page=1").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type, FEED_MEDIA_TYPE);

        assert_eq!(json["metadata"]["numberOfItems"], 5);
        assert_eq!(json["metadata"]["itemsPerPage"], 3);
        assert_eq!(json["metadata"]["currentPage"], 1);
        assert_eq!(json["publications"].as_array().unwrap().len(), 3);

        let rels = rels(&json["links"]);
        assert!(rels.contains(&"first".to_string()));
        assert!(rels.contains(&"last".to_string()));
        assert!(rels.contains(&"next".to_string()));
        assert!(!rels.contains(&"previous".to_string()));

        // Facets expose the category breakdown.
        assert_eq!(json["facets"][0]["metadata"]["title"], "Category");
    }

    #[tokio::test]
    async fn all_publications_last_page_has_previous_not_next() {
        let (_, _, json) = get("/opds/all?page=2").await;
        assert_eq!(json["metadata"]["currentPage"], 2);
        assert_eq!(json["publications"].as_array().unwrap().len(), 2);

        let rels = rels(&json["links"]);
        assert!(rels.contains(&"previous".to_string()));
        assert!(!rels.contains(&"next".to_string()));
    }

    #[tokio::test]
    async fn out_of_range_page_is_clamped() {
        let (status, _, json) = get("/opds/all?page=999").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["metadata"]["currentPage"], 2);
    }

    #[tokio::test]
    async fn category_feed_filters_and_404s_unknown() {
        let (status, _, json) = get("/opds/category/nonfiction").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["metadata"]["numberOfItems"], 2);
        assert_eq!(json["publications"].as_array().unwrap().len(), 2);

        let (status, _, _) = get("/opds/category/bogus").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn publication_document_has_correct_self_link() {
        let (status, content_type, json) = get("/opds/publications/moby-dick").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type, PUBLICATION_MEDIA_TYPE);
        assert_eq!(json["metadata"]["title"], "Moby-Dick; or, The Whale");

        let self_link = json["links"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["rel"] == "self")
            .expect("a self link");
        // Regression guard: self link must include the `/opds` prefix.
        assert_eq!(
            self_link["href"],
            format!("{BASE}/opds/publications/moby-dick")
        );
    }

    #[tokio::test]
    async fn unknown_publication_404s() {
        let (status, _, _) = get("/opds/publications/does-not-exist").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn search_matches_title_author_or_description() {
        let (status, content_type, json) = get("/opds/search?query=whale").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type, FEED_MEDIA_TYPE);
        assert_eq!(json["metadata"]["numberOfItems"], 1);
        assert_eq!(
            json["publications"][0]["metadata"]["title"],
            "Moby-Dick; or, The Whale"
        );
    }

    #[tokio::test]
    async fn empty_search_returns_no_results() {
        let (status, _, json) = get("/opds/search?query=").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["metadata"]["numberOfItems"], 0);
    }

    #[tokio::test]
    async fn open_access_download_serves_an_epub() {
        let (status, content_type, headers, bytes) =
            get_raw("/opds/download/moby-dick.epub").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type, "application/epub+zip");
        assert_eq!(
            headers[header::CONTENT_DISPOSITION],
            "attachment; filename=\"moby-dick.epub\""
        );
        // Zip local-file-header magic, and the OCF mimetype payload.
        assert_eq!(&bytes[..4], b"PK\x03\x04");
        assert!(
            bytes
                .windows(b"application/epub+zip".len())
                .any(|w| w == b"application/epub+zip")
        );
    }

    #[tokio::test]
    async fn paid_title_is_not_open_access_downloadable() {
        let (status, _, _, _) = get_raw("/opds/download/pride-and-prejudice.epub").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unknown_download_404s() {
        let (status, _, _, _) = get_raw("/opds/download/nope.epub").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn buy_page_shows_price() {
        let (status, content_type, _, bytes) = get_raw("/opds/buy/pride-and-prejudice").await;
        assert_eq!(status, StatusCode::OK);
        assert!(content_type.starts_with("text/html"));
        let body = String::from_utf8(bytes).unwrap();
        assert!(body.contains("Pride and Prejudice"));
        assert!(body.contains("$4.99 USD"));
    }

    #[tokio::test]
    async fn cover_and_thumbnail_render_svg() {
        let (status, content_type, _, bytes) = get_raw("/opds/covers/moby-dick.svg").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type, "image/svg+xml");
        let body = String::from_utf8(bytes).unwrap();
        assert!(body.contains("<svg"));
        assert!(body.contains(r#"width="800""#));

        let (status, _, _, bytes) = get_raw("/opds/covers/moby-dick-thumb.svg").await;
        assert_eq!(status, StatusCode::OK);
        let body = String::from_utf8(bytes).unwrap();
        assert!(body.contains(r#"width="160""#));
    }

    #[tokio::test]
    async fn unknown_cover_404s() {
        let (status, _, _, _) = get_raw("/opds/covers/nope.svg").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
