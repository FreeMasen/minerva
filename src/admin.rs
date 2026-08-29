//! A minimal web UI for managing the catalog, mounted under `/admin` (behind
//! auth when it is enabled). Every action maps onto an existing store
//! operation; mutations redirect back to the page (POST/redirect/GET).

use std::path::Path as FsPath;
use std::sync::{Arc, LazyLock};

use axum::{
    Form, Json, Router,
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::catalog::{BookSource, Category};

/// The (autoescaping) Tera templates, compiled once from the embedded sources.
static TEMPLATES: LazyLock<tera::Tera> = LazyLock::new(|| {
    let mut tera = tera::Tera::default();
    tera.add_raw_template("admin.html", include_str!("../templates/admin.html"))
        .expect("admin template compiles");
    tera
});

/// The admin routes (merged into the protected router).
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/admin", get(page))
        .route("/admin/upload", post(upload))
        .route("/admin/books/{id}/properties", post(set_properties))
        .route("/admin/books/{id}/categories", post(add_category))
        .route(
            "/admin/books/{id}/categories/{slug}/delete",
            post(remove_category),
        )
        .route("/admin/books/{id}/delete", post(remove_book))
}

#[derive(Deserialize)]
struct Properties {
    title: String,
    author: String,
    #[serde(default)]
    series: String,
    #[serde(default)]
    series_index: String,
}

#[derive(Deserialize)]
struct CategoryName {
    name: String,
}

/// A downloadable format of a book, as rendered on the admin page.
#[derive(Serialize)]
struct DownloadView {
    label: String,
    href: String,
}

/// A book as rendered on the admin page.
#[derive(Serialize)]
struct BookView {
    id: String,
    title: String,
    author: String,
    series: Option<String>,
    series_index: Option<f64>,
    categories: Vec<Category>,
    downloads: Vec<DownloadView>,
}

/// The management page: an upload form and a row per book.
async fn page(State(state): State<Arc<AppState>>) -> Response {
    let mut books = Vec::new();
    for book in state.catalog.all().await {
        let categories = state.catalog.book_categories(book.id.as_str()).await;
        let downloads = match &book.source {
            BookSource::Files(files) => files
                .iter()
                .map(|f| DownloadView {
                    label: f.format.ext().to_uppercase(),
                    href: format!("/opds/download/{}/{}", book.id, f.format.ext()),
                })
                .collect(),
            BookSource::Sample => Vec::new(),
        };
        books.push(BookView {
            id: book.id.0,
            title: book.title,
            author: book.author,
            series: book.series,
            series_index: book.series_index,
            categories,
            downloads,
        });
    }

    let mut context = tera::Context::new();
    context.insert("count", &books.len());
    context.insert("books", &books);
    context.insert("has_library", &state.library_dir.is_some());

    match TEMPLATES.render("admin.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            tracing::error!(?err, "failed to render admin page");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// The mutation handlers are called by fetch() from the admin page and return a
// bare status (with a plain-text message on error) rather than redirecting, so
// the page updates in place without a reload.

async fn set_properties(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Form(props): Form<Properties>,
) -> Response {
    let title = props.title.trim();
    if title.is_empty() {
        return (StatusCode::BAD_REQUEST, "Title is required.").into_response();
    }
    if state.catalog.get(&id).await.is_none() {
        return (StatusCode::NOT_FOUND, "No such book.").into_response();
    }
    let series = props.series.trim();
    let series = (!series.is_empty()).then_some(series);
    let series_index = props.series_index.trim();
    let series_index = match series_index.is_empty() {
        true => None,
        false => match series_index.parse::<f64>() {
            Ok(n) => Some(n),
            Err(_) => return (StatusCode::BAD_REQUEST, "Series number must be a number.").into_response(),
        },
    };
    let _ = state.catalog.set_title(&id, title).await;
    let _ = state.catalog.set_author(&id, props.author.trim()).await;
    let _ = state.catalog.set_series(&id, series, series_index).await;
    StatusCode::OK.into_response()
}

/// Assign a category and return the canonical (server-slugified) category as
/// JSON, so the client renders the chip with the real slug rather than guessing.
async fn add_category(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Form(body): Form<CategoryName>,
) -> Response {
    let name = body.name.trim();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "Category name is required.").into_response();
    }
    if state.catalog.get(&id).await.is_none() {
        return (StatusCode::NOT_FOUND, "No such book.").into_response();
    }
    match state.catalog.assign_category(&id, name).await {
        Ok(category) => Json(category).into_response(),
        Err(err) => {
            tracing::error!(?err, id, "failed to assign category");
            (StatusCode::INTERNAL_SERVER_ERROR, "Failed to add category.").into_response()
        }
    }
}

async fn remove_category(
    State(state): State<Arc<AppState>>,
    Path((id, slug)): Path<(String, String)>,
) -> Response {
    state.catalog.remove_category(&id, &slug).await;
    StatusCode::NO_CONTENT.into_response()
}

async fn remove_book(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let _ = state.catalog.remove_book(&id).await;
    StatusCode::NO_CONTENT.into_response()
}

/// Save an uploaded EPUB into the library directory and reconcile.
async fn upload(State(state): State<Arc<AppState>>, mut multipart: Multipart) -> Response {
    let Some(dir) = state.library_dir.clone() else {
        return (StatusCode::BAD_REQUEST, "uploads require a library directory").into_response();
    };

    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() != Some("file") {
            continue;
        }
        let Some(filename) = field.file_name().map(sanitize_filename) else {
            continue;
        };
        let data = match field.bytes().await {
            Ok(data) => data,
            Err(err) => {
                tracing::warn!(?err, "failed to read uploaded file");
                continue;
            }
        };
        if let Err(err) = tokio::fs::write(dir.join(&filename), data).await {
            tracing::error!(?err, "failed to save upload");
            return (StatusCode::INTERNAL_SERVER_ERROR, "failed to save upload").into_response();
        }
        state.catalog.reconcile_dir(&dir).await;
    }

    Redirect::to("/admin").into_response()
}

/// Reduce an uploaded filename to a safe basename ending in `.epub`.
fn sanitize_filename(name: &str) -> String {
    let base = FsPath::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("upload.epub");
    if base.to_ascii_lowercase().ends_with(".epub") {
        base.to_string()
    } else {
        format!("{base}.epub")
    }
}
