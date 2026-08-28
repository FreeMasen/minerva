//! A minimal web UI for managing the catalog, mounted under `/admin` (behind
//! auth when it is enabled). Every action maps onto an existing store
//! operation; mutations redirect back to the page (POST/redirect/GET).

use std::path::Path as FsPath;
use std::sync::{Arc, LazyLock};

use axum::{
    Form, Router,
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::catalog::Category;

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
}

#[derive(Deserialize)]
struct CategoryName {
    name: String,
}

/// A book as rendered on the admin page.
#[derive(Serialize)]
struct BookView {
    id: String,
    title: String,
    author: String,
    categories: Vec<Category>,
}

/// The management page: an upload form and a row per book.
async fn page(State(state): State<Arc<AppState>>) -> Response {
    let mut books = Vec::new();
    for book in state.catalog.all().await {
        let categories = state.catalog.book_categories(&book.id).await;
        books.push(BookView {
            id: book.id,
            title: book.title,
            author: book.author,
            categories,
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

async fn set_properties(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Form(props): Form<Properties>,
) -> Response {
    let _ = state.catalog.set_title(&id, props.title.trim()).await;
    let _ = state.catalog.set_author(&id, props.author.trim()).await;
    Redirect::to("/admin").into_response()
}

async fn add_category(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Form(body): Form<CategoryName>,
) -> Response {
    let name = body.name.trim();
    if !name.is_empty() && state.catalog.get(&id).await.is_some() {
        let _ = state.catalog.assign_category(&id, name).await;
    }
    Redirect::to("/admin").into_response()
}

async fn remove_category(
    State(state): State<Arc<AppState>>,
    Path((id, slug)): Path<(String, String)>,
) -> Response {
    state.catalog.remove_category(&id, &slug).await;
    Redirect::to("/admin").into_response()
}

async fn remove_book(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let _ = state.catalog.remove_book(&id).await;
    Redirect::to("/admin").into_response()
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
