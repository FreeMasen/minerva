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
# the catalog is stored in SQLite (default ./opds.db); override the location
OPDS_DB=/var/lib/opds/catalog.db cargo run
# override the base URL used to build absolute hrefs (default http://localhost:3000)
OPDS_BASE_URL=https://books.example.com cargo run
# serve a real library: scan a directory of EPUB files instead of the samples
OPDS_LIBRARY_DIR=/path/to/epubs cargo run
# add an account (stored in the same OPDS_DB); the catalog then requires login
cargo run adduser alice        # prompts for a password (hidden, confirmed)
cargo run                      # HTTP Basic auth is enforced while any account exists
```

Each of these environment variables is also a command-line flag (`--base-url`,
`--db`, `--library-dir`); a flag takes precedence over its variable. Run
`cargo run -- --help` for the full CLI.

The server listens on `0.0.0.0:3000`. Visit http://localhost:3000/opds.

The catalog and the user accounts live in one SQLite database (`OPDS_DB`, in a
`books` and a `users` table). HTTP Basic auth is enforced whenever at least one
account exists, and the catalog is open otherwise.

## Catalog source

The catalog lives in a SQLite `books` table (`OPDS_DB`) and is queried per
request rather than held in memory.

With no `OPDS_LIBRARY_DIR`, the store is (re)seeded with a built-in sample
catalog of public-domain titles (with generated EPUBs and SVG covers).

When `OPDS_LIBRARY_DIR` points at a directory, the server reconciles the store
against it — scanning `*.epub` files recursively and recording each file's
embedded metadata (title, author, language, description, subjects) and cover.
Unchanged files (matching a stored modification time) are skipped on restart, so
startup is cheap for large, mostly-static libraries. The directory is
**watched**: adding an EPUB inserts a row and removing one deletes it — no
restart required. Downloads stream the real file bytes and cover requests serve
the image embedded in the EPUB (falling back to a generated SVG when a book has
no cover).

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
| `GET /opds/auth`               | Authentication document (when auth is enabled).         |

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
- Optional multi-account HTTP Basic authentication (accounts in the `users`
  table of `OPDS_DB`), with Argon2-hashed passwords and constant-time
  verification (RustCrypto). Manage accounts with the `adduser` subcommand;
  auth is enforced whenever an account exists. Protected resources answer 401
  with an Authentication for OPDS document (`application/opds-authentication+json`),
  also served (unprotected) at `/opds/auth`.
- Correct OPDS media types on every response.

## Tests

```sh
cargo test
```

Integration tests drive the fully-wired router (via `tower::ServiceExt::oneshot`)
and cover the root feed, pagination, category filtering, publication documents,
search, EPUB/cover/buy asset endpoints, and 404s. A directory-scan test writes a
generated EPUB to a temp dir and confirms it is picked up and then dropped after
removal. Tests run against an in-memory SQLite database.

## Development

Data access uses [sqlx](https://github.com/launchbadge/sqlx) with compile-time
checked queries. A checked-in offline cache (`.sqlx/`) lets the project build
without a database, so `cargo build` and `cargo test` work out of the box.

If you change any SQL (or the schema in `migrations/`), regenerate the cache:

```sh
export DATABASE_URL=sqlite:dev.db
sqlx database create && sqlx migrate run   # one-time: create the dev database
cargo sqlx prepare                         # refresh .sqlx/ — commit the result
```

## Deployment

A hardened systemd unit is provided at [`opds-axum.service`](opds-axum.service);
its header comments cover installing the binary, creating the service user, and
configuring it (env vars or `/etc/opds-axum/opds-axum.env`). The catalog
database lives in `/var/lib/opds-axum`.

## Layout

- `src/model.rs` — serde types for the OPDS 2.0 wire format.
- `src/catalog.rs` — the `Book`/`Category` domain types, the sample set, and
  EPUB scanning helpers.
- `src/library.rs` — the SQLite-backed catalog store (queries + reconciliation).
- `src/db.rs` — the sqlx connection pool + migrations.
- `migrations/` — SQL schema migrations (applied at startup).
- `src/epub.rs` — reads metadata and cover images out of EPUB files.
- `src/assets.rs` — on-the-fly EPUB and SVG cover generation (for samples and
  cover fallbacks).
- `src/watch.rs` — watches the library directory and updates the catalog store.
- `src/base64.rs` — minimal Base64 for HTTP Basic credentials.
- `src/auth.rs` — the SQLite-backed user store and Argon2 password hashing.
- `src/main.rs` — the Axum router, handlers, auth middleware, and response wrapper.
