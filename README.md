# opds-axum

An [OPDS 2.0](https://specs.opds.io/opds-2.0.html) catalog server built with
[Axum](https://github.com/tokio-rs/axum).

OPDS 2.0 is built on the Readium Web Publication Manifest model: everything is a
JSON *collection* made of `metadata`, `links`, and sub-collections
(`navigation`, `publications`, `facets`, `groups`). Feeds are served as
`application/opds+json` and individual publications as
`application/opds-publication+json`.

## Running

```sh
cargo run
# override the base URL used to build absolute hrefs (default http://localhost:3000)
OPDS_BASE_URL=https://books.example.com cargo run
# serve a real library: scan a directory of EPUB files instead of the samples
OPDS_LIBRARY_DIR=/path/to/epubs cargo run
# protect the catalog with HTTP Basic auth
OPDS_AUTH=user:pass cargo run
```

The server listens on `0.0.0.0:3000`. Visit http://localhost:3000/opds.

## Catalog source

With no `OPDS_LIBRARY_DIR`, the server serves a built-in sample catalog of
public-domain titles (with generated EPUBs and SVG covers).

When `OPDS_LIBRARY_DIR` points at a directory, the server instead scans it for
`*.epub` files and builds the catalog from each file's embedded metadata
(title, author, language, description, subjects) and cover image. The directory
is **watched**: adding an EPUB makes it appear in the feeds and removing one
drops it — no restart required. Downloads stream the real file bytes and cover
requests serve the image embedded in the EPUB (falling back to a generated SVG
when a book has no cover).

## Endpoints

| Method & path                  | Description                                             |
| ------------------------------ | ------------------------------------------------------- |
| `GET /`                        | Redirects to `/opds`.                                   |
| `GET /opds`                    | Root feed: navigation, a "New Publications" **group**, and a browse group. |
| `GET /opds/all?page=N`         | Paginated **acquisition** feed of all publications, with facets and pagination links. |
| `GET /opds/category/{slug}`    | Acquisition feed for a category (`fiction`/`nonfiction`).|
| `GET /opds/publications/{id}`  | A single publication document.                          |
| `GET /opds/search?query=...`   | Search feed; also accepts `author=` and `title=` field filters. |
| `GET /opds/download/{id}.epub` | Open-access download: a generated minimal EPUB 3.       |
| `GET /opds/buy/{id}`           | Advertised for spec completeness; returns 501 (no store).|
| `GET /opds/borrow/{id}`        | Advertised for lendable titles; returns 501 (no lending).|
| `GET /opds/covers/{id}.svg`    | Generated SVG cover (`{id}-thumb.svg` for the thumbnail).|
| `GET /opds/auth`               | Authentication document (when `OPDS_AUTH` is set).      |

## What's implemented

- The core collection model: feeds with `metadata`, `links`, `navigation`,
  `publications`, `facets`, and `groups` (the root feed groups a publications
  preview and a category-browse navigation collection, each with its own
  metadata and `self` link).
- Link objects with `rel`, `type`, `title`, `templated`, and `properties`.
- A filesystem-backed catalog (`OPDS_LIBRARY_DIR`) that scans EPUB files for
  metadata and covers and live-reloads on additions/removals, alongside the
  built-in sample catalog.
- Acquisition links: free `open-access` downloads, paid `buy` links (with a
  `price` and an `indirectAcquisition`), and library `borrow` links carrying
  lending `availability`/`copies`/`holds` (an OPDS extension). Downloads stream
  real EPUB bytes (or a generated minimal EPUB 3 for samples); buy and borrow
  are advertised but report 501.
- Cover `images` (full-size + thumbnail), served from the EPUB's embedded cover
  (thumbnails are downscaled to fit 160x240 and re-encoded as JPEG) or as a
  generated SVG placeholder.
- A templated `search` link (`search{?query,author,title}`) and a search
  endpoint supporting a general query plus per-field author/title filters.
- Pagination on the acquisition feed: `numberOfItems`/`itemsPerPage`/`currentPage`
  metadata plus `first`/`previous`/`next`/`last` links.
- Optional HTTP Basic authentication (`OPDS_AUTH=user:pass`) with an
  Authentication for OPDS document: protected resources answer 401 with an
  `application/opds-authentication+json` challenge, and the document is also
  served (unprotected) at `/opds/auth`.
- Correct OPDS media types on every response.

## Tests

```sh
cargo test
```

Integration tests drive the fully-wired router (via `tower::ServiceExt::oneshot`)
and cover the root feed, pagination, category filtering, publication documents,
search, EPUB/cover/buy asset endpoints, and 404s. A directory-scan test writes a
generated EPUB to a temp dir and confirms it is picked up and then dropped after
removal.

## Layout

- `src/model.rs` — serde types for the OPDS 2.0 wire format.
- `src/catalog.rs` — the `Catalog`/`Book` types, the sample set, and the
  directory scanner.
- `src/epub.rs` — reads metadata and cover images out of EPUB files.
- `src/assets.rs` — on-the-fly EPUB and SVG cover generation (for samples and
  cover fallbacks).
- `src/watch.rs` — watches the library directory and hot-swaps the catalog.
- `src/base64.rs` — minimal Base64 for HTTP Basic credentials.
- `src/main.rs` — the Axum router, handlers, auth middleware, and response wrapper.
