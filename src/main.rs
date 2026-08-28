//! An OPDS 2.0 catalog server built on Axum.
//!
//! Implements the core of the OPDS 2.0 specification
//! (<https://specs.opds.io/opds-2.0.html>): a root navigation feed, acquisition
//! feeds (all publications and per-category), facets, full-text search via a
//! templated link, and individual publication documents.
//!
//! The catalog is either the built-in sample set or a directory of EPUB files
//! (set `OPDS_LIBRARY_DIR`) that is scanned for metadata and covers and kept in
//! sync as files are added or removed.

/// The HTTP Basic realm advertised to clients.
const AUTH_REALM: &str = "OPDS catalog";

/// Number of publications served per page in acquisition feeds.
const PAGE_SIZE: u64 = 3;

mod assets;
mod auth;
mod base64;
mod catalog;
mod db;
mod epub;
mod library;
mod model;
mod watch;

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use anyhow::Context;
use clap::{Parser, Subcommand};
use serde::Deserialize;

use crate::auth::AuthStore;
use crate::catalog::{BookSource, Category};
use crate::library::CatalogStore;
use crate::model::*;

/// An OPDS 2.0 catalog server built on Axum.
#[derive(Parser)]
#[command(name = "opds-axum", version, about)]
struct Cli {
    /// Externally-visible base URL used to build absolute hrefs.
    #[arg(long, short = 'u', env = "OPDS_BASE_URL", default_value = "http://localhost:3000")]
    base_url: String,

    /// SQLite database holding the catalog and user accounts.
    #[arg(long, short, env = "OPDS_DB", default_value = "opds.db", global = true)]
    db: PathBuf,

    /// Directory of EPUB files to serve instead of the built-in samples.
    #[arg(long, short, env = "OPDS_LIBRARY_DIR")]
    library_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Create or update a user account (any account enables HTTP Basic auth).
    Adduser {
        /// The account username.
        username: String,
        /// The password; read from stdin if omitted.
        password: Option<String>,
    },
}

/// Shared application state.
struct AppState {
    /// The externally-visible base URL used to build absolute hrefs.
    base_url: String,
    /// The SQLite-backed catalog, queried per request.
    catalog: Arc<CatalogStore>,
    /// The user store protecting the catalog, if configured.
    auth: Option<Arc<AuthStore>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut cli = Cli::parse();

    match cli.command.take() {
        Some(Command::Adduser { username, password }) => {
            cmd_adduser(&cli.db, &username, password).await
        }
        None => run_server(cli).await,
    }
}

/// Start the catalog server.
async fn run_server(cli: Cli) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "opds_axum=debug,tower_http=debug,info".into()),
        )
        .init();

    // One SQLite database holds both the catalog and the user accounts.
    let pool = db::connect(&cli.db)
        .await
        .with_context(|| format!("opening database {}", cli.db.display()))?;
    let catalog = Arc::new(CatalogStore::new(pool.clone()));

    // A library directory reconciles the catalog against a directory of EPUBs;
    // otherwise we serve the built-in sample set.
    let library_dir = cli.library_dir.filter(|p| !p.as_os_str().is_empty());
    match &library_dir {
        Some(dir) => {
            catalog.remove_sample_books().await;
            catalog.reconcile_dir(dir).await;
        }
        None => {
            tracing::info!("no library directory; serving the built-in sample catalog");
            catalog.reset_to_samples().await;
        }
    }

    // HTTP Basic auth is enforced whenever the user table is non-empty: create
    // an account with `adduser` to lock the catalog, and it's open otherwise.
    let users = AuthStore::new(pool.clone());
    let auth = if users.user_count().await > 0 {
        tracing::info!("HTTP Basic auth enabled");
        Some(Arc::new(users))
    } else {
        None
    };

    // Reflect additions/removals in the library directory as they happen.
    if let Some(dir) = library_dir {
        if let Err(err) = watch::spawn(dir, catalog.clone()) {
            tracing::warn!(%err, "failed to start the library watcher");
        }
    }

    let state = Arc::new(AppState {
        base_url: cli.base_url,
        catalog,
        auth,
    });

    let addr = "0.0.0.0:3000";
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!("OPDS server listening on http://{addr}/opds");
    axum::serve(listener, app(state))
        .await
        .context("server error")?;
    Ok(())
}

/// Create or update an account in the database at `db_path`. When no password
/// is supplied, one is read from stdin.
async fn cmd_adduser(
    db_path: &FsPath,
    username: &str,
    password: Option<String>,
) -> anyhow::Result<()> {
    let password = match password {
        Some(password) => password,
        None => prompt_password()?,
    };
    if password.is_empty() {
        anyhow::bail!("password must not be empty");
    }

    let store = AuthStore::new(
        db::connect(db_path)
            .await
            .with_context(|| format!("opening database {}", db_path.display()))?,
    );
    store.add_user(username, &password, None).await?;
    println!("user '{username}' saved to {}", db_path.display());
    Ok(())
}

/// Prompt for a password on the terminal: hidden input, confirmed by retyping,
/// and rejected if empty.
fn prompt_password() -> anyhow::Result<String> {
    use inquire::Password;
    use inquire::validator::Validation;

    Password::new("Password:")
        .with_validator(|input: &str| {
            if input.trim().is_empty() {
                Ok(Validation::Invalid("Password must not be empty".into()))
            } else {
                Ok(Validation::Valid)
            }
        })
        .prompt()
        .context("reading password")
}

/// Build the application router. Kept separate from `main` so tests can drive
/// the fully-wired app without binding a socket.
fn app(state: Arc<AppState>) -> Router {
    // Catalog routes sit behind the (optional) auth middleware.
    let protected = Router::new()
        .route("/opds", get(root_feed))
        .route("/opds/all", get(all_publications))
        .route("/opds/category/{slug}", get(category_feed))
        .route("/opds/publications/{id}", get(publication))
        .route("/opds/search", get(search))
        .route("/opds/download/{file}", get(download))
        .route("/opds/buy/{id}", get(buy))
        .route("/opds/borrow/{id}", get(borrow))
        .route("/opds/covers/{id}", get(cover))
        .route("/opds/covers/{id}/thumb", get(cover_thumb))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    // The redirect and the authentication document must stay reachable
    // without credentials (the latter is how clients learn how to log in).
    let public = Router::new()
        .route("/", get(root_redirect))
        .route("/opds/auth", get(auth_document));

    public
        .merge(protected)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

/// Middleware that enforces HTTP Basic auth when it is configured. A missing or
/// wrong credential yields a 401 carrying an Authentication for OPDS document.
async fn require_auth(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(store) = state.auth.clone() else {
        return next.run(request).await;
    };

    // `verify` awaits the SQLite lookup and offloads the Argon2 hash to a
    // blocking thread itself.
    let authorized = match extract_basic(request.headers()) {
        Some((user, pass)) => store.verify(&user, &pass).await,
        None => false,
    };

    if authorized {
        next.run(request).await
    } else {
        unauthorized(&state)
    }
}

/// Extract the username and password from an `Authorization: Basic ...` header.
fn extract_basic(headers: &HeaderMap) -> Option<(String, String)> {
    let encoded = headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Basic ")?;
    let decoded = base64::decode(encoded.trim())?;
    let text = String::from_utf8(decoded).ok()?;
    let (user, pass) = text.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

/// The 401 challenge response, with the auth document as its body.
fn unauthorized(state: &AppState) -> Response {
    let body = serde_json::to_vec(&auth_document_body(state)).unwrap_or_default();
    (
        StatusCode::UNAUTHORIZED,
        [
            (
                header::WWW_AUTHENTICATE,
                format!("Basic realm=\"{AUTH_REALM}\""),
            ),
            (header::CONTENT_TYPE, AUTH_MEDIA_TYPE.to_string()),
        ],
        body,
    )
        .into_response()
}

/// Serve the Authentication for OPDS document (only meaningful when auth is on).
async fn auth_document(State(state): State<Arc<AppState>>) -> Response {
    if state.auth.is_some() {
        let body = serde_json::to_vec(&auth_document_body(&state)).unwrap_or_default();
        ([(header::CONTENT_TYPE, AUTH_MEDIA_TYPE)], body).into_response()
    } else {
        not_found("Authentication is not enabled")
    }
}

/// Build the Authentication for OPDS document describing how to log in.
fn auth_document_body(state: &AppState) -> AuthenticationDocument {
    let base = &state.base_url;
    AuthenticationDocument {
        id: format!("{base}/opds/auth"),
        title: AUTH_REALM.to_string(),
        authentication: vec![AuthenticationFlow {
            r#type: "http://opds-spec.org/auth/basic".to_string(),
            labels: Some(AuthLabels {
                login: "Username".to_string(),
                password: "Password".to_string(),
            }),
        }],
        links: vec![
            Link::new(format!("{base}/opds"))
                .with_rel("start")
                .with_type(FEED_MEDIA_TYPE),
        ],
    }
}

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

/// The root feed: the catalog entry point. It carries a top-level navigation
/// link, a "New Publications" group previewing recent titles, and a "Browse by
/// Category" navigation group.
async fn root_feed(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let base = &state.base_url;
    let total = state.catalog.count().await;
    let recent = state.catalog.recent(PAGE_SIZE).await;

    let mut feed = Feed::new("Example OPDS Catalog", format!("{base}/opds"))
        .with_link(
            Link::new(format!("{base}/opds"))
                .with_rel("start")
                .with_type(FEED_MEDIA_TYPE),
        )
        .with_link(
            // A templated search link supporting a general query plus optional
            // author/title fields, per the OPDS search convention.
            Link::new(format!("{base}/opds/search{{?query,author,title}}"))
                .with_rel("search")
                .with_type(FEED_MEDIA_TYPE)
                .with_title("Search the catalog")
                .templated(),
        );

    feed.metadata.description =
        Some("A sample catalog demonstrating OPDS 2.0 over Axum.".to_string());

    if state.auth.is_some() {
        feed = feed.with_link(
            Link::new(format!("{base}/opds/auth"))
                .with_rel("http://opds-spec.org/auth/document")
                .with_type(AUTH_MEDIA_TYPE),
        );
    }

    feed.navigation = vec![
        Link::new(format!("{base}/opds/all"))
            .with_rel("http://opds-spec.org/sort/new")
            .with_type(FEED_MEDIA_TYPE)
            .with_title("All Publications"),
    ];

    // A "New Publications" group: a short preview of publications whose `self`
    // link resolves to the full acquisition feed.
    let mut new_meta = Metadata::new("New Publications");
    new_meta.number_of_items = Some(total);
    let new_group = Group {
        metadata: new_meta,
        links: vec![
            Link::new(format!("{base}/opds/all"))
                .with_rel("self")
                .with_type(FEED_MEDIA_TYPE),
        ],
        navigation: Vec::new(),
        publications: recent.iter().map(|b| b.to_publication(base)).collect(),
    };

    // A "Browse by Category" group: a navigation collection of category feeds.
    let browse_group = Group {
        metadata: Metadata::new("Browse by Category"),
        links: Vec::new(),
        navigation: vec![
            Link::new(format!("{base}/opds/category/fiction"))
                .with_rel("subsection")
                .with_type(FEED_MEDIA_TYPE)
                .with_title("Fiction"),
            Link::new(format!("{base}/opds/category/nonfiction"))
                .with_rel("subsection")
                .with_type(FEED_MEDIA_TYPE)
                .with_title("Non-Fiction"),
        ],
        publications: Vec::new(),
    };

    feed.groups = vec![new_group, browse_group];

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
    let total = state.catalog.count().await;

    // At least one page even when the catalog is empty.
    let last_page = total.div_ceil(PAGE_SIZE).max(1);
    let page = params.page.unwrap_or(1).clamp(1, last_page);

    let page_books = state.catalog.page(PAGE_SIZE, (page - 1) * PAGE_SIZE).await;

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
    feed.facets = vec![category_facet(base, &state.catalog).await];

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

    let books = state.catalog.by_category(category).await;

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
    match state.catalog.get(&id).await {
        Some(book) => Opds::publication(book.to_publication(&state.base_url)).into_response(),
        None => not_found("No such publication"),
    }
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    /// A general query matched against title, author and description.
    #[serde(default)]
    query: String,
    /// An optional filter matched against the author only.
    author: Option<String>,
    /// An optional filter matched against the title only.
    title: Option<String>,
}

/// A search feed. `query` matches title/author/description; the optional
/// `author` and `title` fields further constrain the results (all supplied
/// terms must match).
async fn search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    let base = &state.base_url;

    let query = params.query.trim().to_lowercase();
    let author = params.author.as_deref().unwrap_or_default().trim().to_lowercase();
    let title = params.title.as_deref().unwrap_or_default().trim().to_lowercase();

    let matches = state.catalog.search(&query, &author, &title).await;

    // Echo the supplied parameters back in the self link.
    let mut self_href = format!("{base}/opds/search?query={}", urlencode(&params.query));
    if let Some(author) = &params.author {
        self_href.push_str(&format!("&author={}", urlencode(author)));
    }
    if let Some(title) = &params.title {
        self_href.push_str(&format!("&title={}", urlencode(title)));
    }

    let mut feed = Feed::new("Search results", self_href).with_link(
        Link::new(format!("{base}/opds"))
            .with_rel("start")
            .with_type(FEED_MEDIA_TYPE),
    );

    feed.metadata.number_of_items = Some(matches.len() as u64);
    feed.publications = matches.iter().map(|b| b.to_publication(base)).collect();

    Opds::feed(feed)
}

/// Serve the open-access EPUB for a publication. The path carries the filename
/// (`{id}.epub`); we strip the extension to recover the book id. Sample books
/// get a generated EPUB; file-backed books stream their bytes from disk.
async fn download(State(state): State<Arc<AppState>>, Path(file): Path<String>) -> Response {
    let id = file.strip_suffix(".epub").unwrap_or(&file).to_string();

    let Some(book) = state.catalog.get(&id).await else {
        return not_found("No such publication");
    };

    match &book.source {
        BookSource::Sample => {
            if !book.is_open_access() {
                return not_found("This publication is not available for open-access download");
            }
            epub_response(&format!("{id}.epub"), assets::epub_bytes(&book))
        }
        BookSource::File { path } => {
            let filename = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("book.epub")
                .to_string();
            let path = path.clone();
            match tokio::fs::read(&path).await {
                Ok(bytes) => epub_response(&filename, bytes),
                Err(err) => {
                    tracing::error!(%err, path = %path.display(), "failed to read EPUB file");
                    not_found("Publication file is unavailable")
                }
            }
        }
    }
}

/// Build an EPUB download response with an attachment filename.
fn epub_response(filename: &str, bytes: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/epub+zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        bytes,
    )
        .into_response()
}

/// The `buy` acquisition link is advertised (with price and indirect
/// acquisition) for spec completeness, but this server is not a store. An
/// unknown id still 404s; a real publication reports 501 Not Implemented.
async fn buy(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Some(book) = state.catalog.get(&id).await else {
        return not_found("No such publication");
    };

    let price = book
        .price_usd
        .map(|p| format!("${p:.2} USD"))
        .unwrap_or_else(|| "an unlisted price".to_string());
    let message = format!(
        "Purchasing is not available on this server. \"{}\" is listed at {price}.",
        book.title,
    );
    (StatusCode::NOT_IMPLEMENTED, message).into_response()
}

/// Like `buy`, the `borrow` acquisition link is advertised (with availability
/// and copy/hold counts) for completeness, but lending is not implemented.
async fn borrow(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Some(book) = state.catalog.get(&id).await else {
        return not_found("No such publication");
    };
    let message = format!(
        "Borrowing is not available on this server. \"{}\" is listed as lendable.",
        book.title,
    );
    (StatusCode::NOT_IMPLEMENTED, message).into_response()
}

/// Serve a book's full-size cover.
async fn cover(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    serve_cover(state, id, false).await
}

/// Serve a book's thumbnail cover.
async fn cover_thumb(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    serve_cover(state, id, true).await
}

/// Serve a cover image: the real embedded image for file-backed books, or a
/// generated SVG placeholder when there is no embedded cover (or it can't be
/// read).
async fn serve_cover(state: Arc<AppState>, id: String, thumbnail: bool) -> Response {
    let Some(book) = state.catalog.get(&id).await else {
        return not_found("No such cover");
    };

    if let (BookSource::File { path }, Some(cover)) = (&book.source, &book.cover) {
        let path = path.clone();
        let zip_path = cover.zip_path.clone();
        let media_type = cover.media_type.clone();
        // Read the embedded cover (and, for a thumbnail, downscale it) off the
        // async runtime — both zip reads and image decoding are blocking.
        let read = tokio::task::spawn_blocking(move || {
            let bytes = epub::read_entry(&path, &zip_path)?;
            if thumbnail {
                if let Some(thumb) = assets::thumbnail(&bytes, 160, 240) {
                    return Ok((thumb, "image/jpeg".to_string()));
                }
            }
            Ok::<_, std::io::Error>((bytes, media_type))
        })
        .await;

        match read {
            Ok(Ok((bytes, content_type))) => {
                return ([(header::CONTENT_TYPE, content_type)], bytes).into_response();
            }
            Ok(Err(err)) => {
                tracing::warn!(%err, id, "failed to read embedded cover; using placeholder");
            }
            Err(err) => {
                tracing::error!(%err, "cover read task panicked");
            }
        }
    }

    let (width, height) = if thumbnail { (160, 240) } else { (800, 1200) };
    let svg = assets::cover_svg(&book, width, height);
    ([(header::CONTENT_TYPE, "image/svg+xml")], svg).into_response()
}

/// Build a "Category" facet group linking to each category feed.
async fn category_facet(base: &str, catalog: &CatalogStore) -> Facet {
    let facet_link = |category: Category, count: u64| {
        Link::new(format!("{base}/opds/category/{}", category.slug()))
            .with_type(FEED_MEDIA_TYPE)
            .with_title(category.label())
            .with_properties(LinkProperties {
                number_of_items: Some(count),
                ..Default::default()
            })
    };

    Facet {
        metadata: Metadata::new("Category"),
        links: vec![
            facet_link(Category::Fiction, catalog.count_category(Category::Fiction).await),
            facet_link(
                Category::NonFiction,
                catalog.count_category(Category::NonFiction).await,
            ),
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

    /// An in-memory catalog store seeded with the sample books.
    async fn sample_store() -> Arc<CatalogStore> {
        let store = CatalogStore::new(db::connect_memory().await.unwrap());
        store.reset_to_samples().await;
        Arc::new(store)
    }

    async fn test_app() -> Router {
        app(Arc::new(AppState {
            base_url: BASE.to_string(),
            catalog: sample_store().await,
            auth: None,
        }))
    }

    async fn test_app_with_auth() -> Router {
        // Catalog and users share one database, as in production.
        let pool = db::connect_memory().await.unwrap();
        let catalog = CatalogStore::new(pool.clone());
        catalog.reset_to_samples().await;
        let users = auth::AuthStore::new(pool);
        users.add_user("admin", "secret", None).await.unwrap();
        app(Arc::new(AppState {
            base_url: BASE.to_string(),
            catalog: Arc::new(catalog),
            auth: Some(Arc::new(users)),
        }))
    }

    /// Issue a GET and return the status, content-type, and parsed JSON body
    /// (`Value::Null` if the body is not JSON).
    async fn get(uri: &str) -> (StatusCode, String, Value) {
        let response = test_app()
            .await
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
            .await
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
            .await
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
        assert_eq!(
            search["href"],
            format!("{BASE}/opds/search{{?query,author,title}}")
        );
    }

    #[tokio::test]
    async fn root_feed_has_groups() {
        let (_, _, json) = get("/opds").await;
        let groups = json["groups"].as_array().expect("groups array");
        assert_eq!(groups.len(), 2);

        // A publications group previewing recent titles, with a self link.
        let new = &groups[0];
        assert_eq!(new["metadata"]["title"], "New Publications");
        assert_eq!(new["metadata"]["numberOfItems"], 5);
        assert!(!new["publications"].as_array().unwrap().is_empty());
        assert!(
            new["links"]
                .as_array()
                .unwrap()
                .iter()
                .any(|l| l["rel"] == "self")
        );

        // A navigation group listing the category feeds.
        let browse = &groups[1];
        assert_eq!(browse["metadata"]["title"], "Browse by Category");
        assert_eq!(browse["navigation"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn paid_publication_has_indirect_acquisition() {
        let (_, _, json) = get("/opds/publications/pride-and-prejudice").await;
        let buy = json["links"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["rel"] == "http://opds-spec.org/acquisition/buy")
            .expect("a buy link");
        // The buy link resolves to an HTML page; the file is acquired indirectly.
        assert_eq!(buy["type"], "text/html");
        assert_eq!(buy["properties"]["price"]["value"], 4.99);
        assert_eq!(
            buy["properties"]["indirectAcquisition"][0]["type"],
            "application/epub+zip"
        );
    }

    #[tokio::test]
    async fn lendable_publication_has_borrow_with_availability() {
        let (_, _, json) = get("/opds/publications/the-art-of-war").await;
        let borrow = json["links"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["rel"] == "http://opds-spec.org/acquisition/borrow")
            .expect("a borrow link");
        assert_eq!(borrow["type"], "text/html");
        let props = &borrow["properties"];
        assert_eq!(props["availability"]["state"], "available");
        assert_eq!(props["copies"]["total"], 3);
        assert_eq!(props["copies"]["available"], 2);
        assert_eq!(props["holds"]["total"], 1);
        assert_eq!(props["indirectAcquisition"][0]["type"], "application/epub+zip");

        // A lendable title has no buy or open-access link, and its download
        // endpoint refuses.
        assert!(
            json["links"]
                .as_array()
                .unwrap()
                .iter()
                .all(|l| l["rel"] != "http://opds-spec.org/acquisition/open-access")
        );
        let (status, _, _, _) = get_raw("/opds/download/the-art-of-war.epub").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn borrow_endpoint_is_not_implemented() {
        let (status, _, _, bytes) = get_raw("/opds/borrow/the-art-of-war").await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert!(String::from_utf8(bytes).unwrap().contains("The Art of War"));

        let (status, _, _, _) = get_raw("/opds/borrow/does-not-exist").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn search_filters_by_author_and_title_fields() {
        // Author-only filter.
        let (_, _, json) = get("/opds/search?author=austen").await;
        assert_eq!(json["metadata"]["numberOfItems"], 1);
        assert_eq!(
            json["publications"][0]["metadata"]["title"],
            "Pride and Prejudice"
        );

        // Title-only filter.
        let (_, _, json) = get("/opds/search?title=war").await;
        assert_eq!(json["metadata"]["numberOfItems"], 1);
        assert_eq!(json["publications"][0]["metadata"]["title"], "The Art of War");

        // A query and an author filter that disagree yield nothing.
        let (_, _, json) = get("/opds/search?query=whale&author=austen").await;
        assert_eq!(json["metadata"]["numberOfItems"], 0);
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
    async fn buy_endpoint_is_not_implemented() {
        let (status, _, _, bytes) = get_raw("/opds/buy/pride-and-prejudice").await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        let body = String::from_utf8(bytes).unwrap();
        assert!(body.contains("Pride and Prejudice"));
        assert!(body.contains("$4.99 USD"));

        // An unknown id still 404s rather than reporting 501.
        let (status, _, _, _) = get_raw("/opds/buy/does-not-exist").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cover_and_thumbnail_render_svg() {
        let (status, content_type, _, bytes) = get_raw("/opds/covers/moby-dick").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type, "image/svg+xml");
        let body = String::from_utf8(bytes).unwrap();
        assert!(body.contains("<svg"));
        assert!(body.contains(r#"width="800""#));

        let (status, _, _, bytes) = get_raw("/opds/covers/moby-dick/thumb").await;
        assert_eq!(status, StatusCode::OK);
        let body = String::from_utf8(bytes).unwrap();
        assert!(body.contains(r#"width="160""#));
    }

    #[tokio::test]
    async fn unknown_cover_404s() {
        let (status, _, _, _) = get_raw("/opds/covers/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // A generated EPUB, when written to a directory and reconciled into the
    // store, is recognized and its metadata recovered — and a later reconcile
    // reflects removal.
    #[tokio::test]
    async fn reconcile_reflects_add_and_remove() {
        use std::fs;

        let dir = std::env::temp_dir().join(format!("opds-scan-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Reuse a sample book to synthesize a real EPUB on disk.
        let store = CatalogStore::new(db::connect_memory().await.unwrap());
        store.reset_to_samples().await;
        let book = store.get("moby-dick").await.unwrap();
        fs::write(dir.join("first.epub"), assets::epub_bytes(&book)).unwrap();

        // Reconcile clears samples (removed by the caller) and picks up the file.
        store.remove_sample_books().await;
        store.reconcile_dir(&dir).await;
        assert_eq!(store.count().await, 1);
        let found = &store.all().await[0];
        assert_eq!(found.title, book.title);
        assert_eq!(found.author, book.author);
        assert!(matches!(found.source, BookSource::File { .. }));

        // Removing the file and reconciling drops the book.
        fs::remove_file(dir.join("first.epub")).unwrap();
        store.reconcile_dir(&dir).await;
        assert_eq!(store.count().await, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn thumbnail_downscales_and_reencodes_as_jpeg() {
        // A 400x600 solid PNG (2:3 aspect, same as the 160x240 thumbnail box).
        let source = image::RgbImage::from_pixel(400, 600, image::Rgb([200, 30, 30]));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(source)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();

        let thumb = assets::thumbnail(png.get_ref(), 160, 240).expect("a thumbnail");
        // Valid JPEG, downscaled, aspect ratio preserved.
        assert_eq!(&thumb[..3], b"\xff\xd8\xff");
        let decoded = image::load_from_memory(&thumb).expect("decodable");
        assert_eq!((decoded.width(), decoded.height()), (160, 240));

        // Non-image bytes yield no thumbnail (caller falls back).
        assert!(assets::thumbnail(b"not an image", 160, 240).is_none());
    }

    #[tokio::test]
    async fn auth_store_hashes_and_verifies() {
        let store = auth::AuthStore::new(db::connect_memory().await.unwrap());
        store
            .add_user("alice", "correct horse", Some("Alice"))
            .await
            .unwrap();

        assert_eq!(store.user_count().await, 1);
        assert!(store.verify("alice", "correct horse").await);
        assert!(!store.verify("alice", "wrong password").await);
        assert!(!store.verify("nobody", "correct horse").await);

        // add_user upserts: the password can be changed in place.
        store.add_user("alice", "new passphrase", None).await.unwrap();
        assert_eq!(store.user_count().await, 1);
        assert!(store.verify("alice", "new passphrase").await);
        assert!(!store.verify("alice", "correct horse").await);
    }

    #[test]
    fn base64_round_trips() {
        for sample in ["admin:secret", "", "a", "ab", "abc", "user:p@ss:word"] {
            let encoded = base64::encode(sample.as_bytes());
            assert_eq!(base64::decode(&encoded).unwrap(), sample.as_bytes());
        }
    }

    #[tokio::test]
    async fn protected_catalog_challenges_without_credentials() {
        let response = test_app_with_auth()
            .await
            .oneshot(Request::builder().uri("/opds").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()[header::CONTENT_TYPE], AUTH_MEDIA_TYPE);
        assert!(response.headers().contains_key(header::WWW_AUTHENTICATE));

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json["authentication"][0]["type"],
            "http://opds-spec.org/auth/basic"
        );
    }

    #[tokio::test]
    async fn protected_catalog_accepts_valid_credentials() {
        let credentials = base64::encode(b"admin:secret");
        let response = test_app_with_auth()
            .await
            .oneshot(
                Request::builder()
                    .uri("/opds")
                    .header(header::AUTHORIZATION, format!("Basic {credentials}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn protected_catalog_rejects_wrong_password() {
        let credentials = base64::encode(b"admin:wrong");
        let response = test_app_with_auth()
            .await
            .oneshot(
                Request::builder()
                    .uri("/opds")
                    .header(header::AUTHORIZATION, format!("Basic {credentials}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_document_is_reachable_without_credentials() {
        let response = test_app_with_auth()
            .await
            .oneshot(Request::builder().uri("/opds/auth").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], AUTH_MEDIA_TYPE);
    }

    // Books in Fiction/ and Non-Fiction/ subfolders are found recursively and
    // categorized by their folder (the generated EPUBs carry no subjects, so
    // this specifically exercises the folder override).
    #[tokio::test]
    async fn recursive_scan_categorizes_by_subfolder() {
        use std::fs;

        let dir = std::env::temp_dir().join(format!("opds-recur-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("Fiction")).unwrap();
        fs::create_dir_all(dir.join("Non-Fiction")).unwrap();

        let samples = CatalogStore::new(db::connect_memory().await.unwrap());
        samples.reset_to_samples().await;
        let fiction = samples.get("moby-dick").await.unwrap();
        let nonfiction = samples.get("the-art-of-war").await.unwrap();
        fs::write(dir.join("Fiction/a.epub"), assets::epub_bytes(&fiction)).unwrap();
        fs::write(dir.join("Non-Fiction/b.epub"), assets::epub_bytes(&nonfiction)).unwrap();

        let store = CatalogStore::new(db::connect_memory().await.unwrap());
        store.reconcile_dir(&dir).await;
        assert_eq!(store.count().await, 2);
        for book in store.all().await {
            if book.title.contains("Moby") {
                assert_eq!(book.category, Category::Fiction);
            } else if book.title.contains("Art of War") {
                assert_eq!(book.category, Category::NonFiction);
            } else {
                panic!("unexpected book: {}", book.title);
            }
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
