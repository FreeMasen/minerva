# Plan / deferred work

All previously-deferred items have been implemented. Kept here as a record.

## Done

- **Watcher scalability** — scanning is recursive and the watcher updates
  incrementally (only changed EPUBs are re-read), falling back to a full rescan
  for directory-level changes. (`src/watch.rs`, `src/catalog.rs`)
- **Thumbnail resizing** — `/opds/covers/{id}/thumb` downscales the embedded
  cover to fit 160x240 and re-encodes as JPEG. (`assets::thumbnail`)
- **Category heuristic** — categories prefer a top-level `Fiction/` or
  `Non-Fiction/` library subfolder, with a broadened subject fallback.
  (`Category::from_path` / `classify`)
- **Authentication for OPDS** — optional HTTP Basic (`OPDS_AUTH=user:pass`),
  401 challenge with an `application/opds-authentication+json` document, served
  publicly at `/opds/auth`. (`src/main.rs`, `src/base64.rs`)
- **Library availability** — `availability` / `holds` / `copies` on `borrow`
  acquisition links (an OPDS extension). (`src/model.rs`, `src/catalog.rs`)

## Possible future work (not requested)

- Real purchase/borrow flows (currently 501) with entitlement + gated delivery.
- Constant-time credential comparison; token/OAuth auth flows.
- Larger-library performance: stream downloads instead of buffering; cache
  extracted covers.
