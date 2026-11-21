# Architecture

paperjam is one Rust binary: an HTTP server, two pools of background workers and a set of command-line tools, all working on a single data directory. This page is for contributors, and for operators who want to know what runs where, what ends up on disk and what happens to a file after it is uploaded. It describes the code as it is. Settings are in [configuration](configuration.md).

```
        browser (web app)             MCP clients (Claude.ai, Claude Code, stdio proxy)
               │                                  │
     REST /api/v1 (cookie | Bearer)        /mcp (OAuth | Bearer)
               └────────────────┬─────────────────┘
                         service layer
        ┌───────────┬───────────┴┬─────────────────┐
     SQLite      file store    tantivy index     job queue (SQLite table)
   (source of   (originals,    (derived,           │
     truth)     derivatives)   rebuildable)   worker pools
                                              ├─ OCR pool: extract → OCR → MRZ → checks → index
                                              └─ LLM pool: transcribe → classify → fields → checks → index
                                                          │                │
                                                    tesseract CLI      LLM API (OpenRouter
                                                                       or any OpenAI-compatible)
```

The crate is a library plus a binary. A small `xtask` crate generates the synthetic test corpus. The web app is a Vite build embedded into the binary, so there is nothing else to deploy.

| Where | What lives there |
|---|---|
| `src/service` | All business logic: documents, upload, taxonomy, users, tokens, passkeys, shares, links, calendar feed, backup, export, Paperless import |
| `src/http` | The REST adapter, sign-in and session handling, the CSRF check, rate limiting, the OpenAPI document, the share page, the calendar route and the embedded web app |
| `src/mcp`, `src/oauth` | The MCP adapter (tool contract in `tools.json`, stdio proxy) and the OAuth server MCP clients sign in through |
| `src/pipeline`, `src/extract`, `src/llm` | Processing: the local phase and checks, PDF and image handling with Tesseract, and the LLM client with its prompts, schemas and budget |
| `src/search`, `src/jobs`, `src/db` | The tantivy index and query language, the job queue and workers, SQLite access and migrations |
| `src/validators`, `src/mrz.rs` | Identifier checks, described in [validators](validators.md) |
| `src/cli`, `src/inbox.rs`, `src/email` | The subcommands, the watched inbox folder, and inbound email parsing and ingestion |

## Layers

**The service layer** (`src/service`) owns the business logic. REST (axum) and MCP (rmcp) are thin adapters over the same functions, and each call runs in one transaction. Errors are typed: `NotFound`, `Conflict`, `Invalid`, `Unauthorized`, `Forbidden`, `Unavailable`, `Upstream`, `Internal`, `RateLimited` and `PayloadTooLarge`. Each carries a stable machine code such as `duplicate` or `file_too_large`, and each transport maps the kind its own way; REST answers `404`, `409`, `422`, `401`, `403`, `503`, `502`, `500`, `429` and `413`, with a body of `{"code", "message", "details"}`. A test (`tests/mcp_parity.rs`) keeps the MCP tools and the REST routes in step.

**Authentication** is in `src/http/auth.rs` and `src/oauth`. The web app uses a session cookie, which starts from a password or a passkey. Passwords are hashed with Argon2, and at most three hashes are computed at once so a flood of sign-in attempts queues instead of exhausting memory. Passkeys (`src/service/passkeys.rs`) use WebAuthn through `webauthn-rs`, with the relying party taken from `PAPERJAM_PUBLIC_URL` and single-use challenges held in memory for five minutes. They are managed with a cookie session only.

The REST API accepts the cookie or an API token (`pj_…`). `/mcp` accepts an API token or an OAuth access token (`pjo_…`). OAuth is an authorization-code flow with PKCE and open Dynamic Client Registration; a client that registers itself is inert until a signed-in user approves it on the consent page. Creating users and issuing API tokens needs a web session (the CLI works too), so a stolen token cannot mint more tokens. The first administrator is created on the web page with a setup code derived from the secret key and printed in the server log (or by `PAPERJAM_ADMIN_USER`/`PAPERJAM_ADMIN_PASSWORD`, or with the CLI); until a user exists the API and MCP refuse every request with `no_users` (the two setup endpoints excepted). The setup insert is conditional on an empty users table, so concurrent requests cannot both succeed, and wrong codes are rate limited like wrong passwords.

**Agents cannot purge.** Purging needs a cookie session and a document that is already in the trash. The check is in the service layer, so no transport can skip it. Clearing a document's *Never send to the cloud* switch needs a web session for the same reason: a token, or an agent misled by a document, can turn the switch on but not off. A reprocess may only name a model from the allowed list.

## Data directory

`PAPERJAM_DATA_DIR` holds everything the server owns:

| Path | What | Rebuildable? |
|---|---|---|
| `paperjam.db` | SQLite in WAL mode: documents, fields, taxonomy, users, tokens, passkeys, jobs, audit events, LLM usage, share links, the calendar feed | no |
| `originals/<aa>/<sha256>` | The uploaded files, byte for byte, named by their SHA-256 | no |
| `derived/<document id>/` | Page renders (`<n>.webp`), the same pages before cleaning (`original-<n>.webp`), `thumbnail.webp` and, for scans, `clean.pdf` | yes, by reprocessing |
| `index/` | The tantivy search index | yes, by `paperjam reindex` or at the next start |
| `tmp/` | Uploads being received | yes |

SQLite has one writer connection and a small pool of readers. Migrations are embedded in the binary and applied at start; a snapshot test of `src/db/schema.txt` catches accidental schema drift.

An original is never modified: no conversion, no metadata injection, no text overlay. `GET /api/v1/documents/{id}/original` and `get_download_link` return exactly the bytes that were uploaded. The web app's **Download** button gives a scan as its clean copy instead (see [Local phase](#local-phase)), and a share link does the same.

Upload and purge of the same bytes take one lock per hash, and a blob is moved into `originals/` only while holding it. A purge that runs at the same moment therefore cannot delete the blob between "stored" and "recorded".

**Do not put files into `originals/` by hand, and never start the server on originals without their database.** At every start the server deletes each file in `originals/` that the database does not list, and empties `tmp/`. That is what cleans up after a crash in the middle of an upload. It also means that a start with a missing or empty `paperjam.db` next to a full `originals/` deletes every original. This happens, for instance, when a volume is mounted for one and not the other. Keep the two together and restore them together, which is what `paperjam restore` does.

### Start and stop

At start the server applies migrations, seeds the built-in document types, puts jobs left `running` by a crash back in the queue, removes unlisted files and expired web sessions, and rebuilds the search index if it holds a different number of documents than the database. Then it starts the workers and, if configured, the inbox scanner, and begins serving requests.

On `SIGTERM` (`docker stop`) or Ctrl-C it stops taking requests, stops the workers and the scanner, and writes a final index commit before exiting.

## Processing

A document is processed by jobs in the `jobs` table (kind, document, attempts, `run_after`, last error). The queue survives restarts. A failed job is retried with exponential backoff, `min(10 s · 2^attempts, 1 h)`, and parked as `dead` after five attempts. Each failed attempt marks the document `failed`, with the error code as its reason and the `processing_failed` flag, and a retry takes it back through `processing`. A job found `running` after a crash is reclaimed at start and still counts as an attempt.

A document moves through four statuses: `queued`, `processing`, `ready` and `failed`. A reprocess sends a `ready` document back to `queued`.

### Two worker pools

Local recognition is CPU-bound and fast, while the LLM is network-bound and slow. With one pool, a slow API call would hold up the OCR of the next upload, so there are two:

- the **OCR pool** (`PAPERJAM_OCR_WORKERS`) runs the local phase of a `Process` job, plus `Reindex` jobs and `Relink` jobs, which match people against stored documents after the people changed (one queued request covers any number of changes);
- the **LLM pool** (`PAPERJAM_LLM_WORKERS`) runs `Process` jobs marked LLM-only.

A new document is queued as local plus LLM. The OCR pool does the local phase, so the document is `ready` and searchable right away. It then enqueues a second, LLM-only job for the LLM pool, unless a newer `Process` job for the same document is already waiting, because enqueueing would overwrite its payload. The LLM-only run takes the status through `processing` and back to `ready`.

### Local phase

1. For each PDF page, the embedded text is extracted and scored: the share of expected letters, replacement characters, absurd token lengths. Good text is kept as the `embedded` layer. Pages with bad or no text go to OCR.
2. Images and OCR-bound pages are decoded or rendered at 300 DPI, with EXIF orientation applied and the page capped at A4 size at that resolution. Tesseract's orientation detection (`osd`) gives the rotation. On the first two pages, where an identity document is likely to be, an unsure verdict is checked by reading a smaller copy of the page at each possible turn and keeping the one that reads best. The page is then deskewed and, if it is a card or sheet on a background, cut out. Each page logs where its time went (`page processed`).
3. Tesseract runs as a subprocess per page, which isolates it, returns its memory to the operating system and lets it be timed out. It yields text and word boxes as the `tesseract` layer.
4. A page that shows MRZ-like filler lines gets a second Tesseract pass over its bottom part with a restricted alphabet, because the multi-language pass turns `<` and Latin letters into Cyrillic lookalikes. A zone whose check digits all pass fills the name, number, dates and nationality fields with provenance `mrz` and sets the document type. A zone that fails its check digits fills **nothing**; it is stored as the fact `mrz_check_digits=failed`, from which the `checksum_failed` flag is derived.
5. Local alias matching links people and correspondents to the document. The checks recompute the attention flags, and the derived files and the search index are written.

For a scan (an image, or a PDF whose pages were all read), step 2 also produces a **clean copy**: each page upright, cut out of the sheet and straightened, encoded as JPEG and bound into one PDF, `clean.pdf`. The viewer shows these pages, the web app's Download button and the share page offer the clean copy, and the page as it was read stays as `original-<n>.webp`. The original file is untouched.

The results of a run are written in one transaction after all pages are done, so a document never shows half a run.

### Attention flags

A document is flagged when an automatic check fails, not when a person would like to look at it. There are six flags. Derived flags are recomputed from the stored values every time, so editing the affected field clears them. Carried flags record something that happened during processing and stay until the next reprocess redoes that work.

| Flag | Kind | Raised when |
|---|---|---|
| `checksum_failed` | derived | An identifier with a known checksum fails it, or a scanned MRZ failed its check digits. Values you typed yourself are never checked. |
| `ocr_llm_disagree` | derived | Digits of a money or identifier value that the model read do not appear in the Tesseract text of the page they came from. Pages without a Tesseract layer cannot be checked. |
| `mrz_mismatch` | carried | The model disagreed with a value read from a valid MRZ. The MRZ value stays. |
| `llm_failed` | carried | An LLM step failed. |
| `llm_budget` | carried | The monthly budget ran out before an LLM step. |
| `processing_failed` | carried | The pipeline failed; the document is `failed`. |

### LLM phase

There are three steps. Each is skipped for documents marked *Never send to the cloud* and when no API key is set.

- **A** transcribes scanned pages from images that paperjam renders. Born-digital pages are not transcribed, because their text is exact.
- **B** classifies the document and fills its metadata. For a born-digital document it also receives a small render of page 1, because an issuer can be a logo that the text layer lacks.
- **C** fills the typed fields of the chosen type, plus extra facts.

Every answer is validated against the request schema at run time, and the model picks people from an enum of ids, so it cannot invent one.

The phase **soft-fails**. Any error ends it, keeps the steps that already finished and adds `llm_failed`, or `llm_budget` when the monthly budget ran out before a step. Local results are never lost and no partial answer is written. The budget is checked before each step, and a billed but unusable answer still has its cost booked. Values with provenance `manual` are never overwritten, and neither are `mrz` values: if the model disagrees with one, the MRZ value stays and `mrz_mismatch` is raised.

Document text is untrusted. The prompts frame it between per-request random delimiters, and the model's output can only fill schema fields.

### PDF stack

PDF handling sits behind two traits, `TextExtractor` and `PageRenderer` in `src/extract`. The implementations are `pdf_oxide` for text and `hayro` for rasterizing. Both are pure Rust and young, so the traits exist to make a swap to pdfium cheap. They have an "open a document, then ask for page N" shape, which avoids parsing the file again for every page.

Encrypted PDFs fail cleanly with `pdf_encrypted`, and unreadable ones with `pdf_corrupt`. Page count and pixel count are capped (300 pages, 100 megapixels per image) as a defense against decompression bombs.

### Parsing in a child process

PDF and image parsers (`pdf_oxide`, `hayro`, the `image` crate, `libheif`) read files that third parties send, so they never run inside the server. For each document the local phase starts `paperjam extract-worker` (the same binary) and hands it the file on stdin. The worker answers on stdout with framed messages: the embedded text of every page, then rendered pages as raw RGB, or a decoded image. The protocol is documented in `src/extract/worker/wire.rs`.

A hang, a decompression bomb, a crash or memory corruption in a parser ends that one process and fails that one document with the code a corrupt file always got (`pdf_corrupt` or `image_corrupt`). The server keeps serving.

- **Time.** The worker is killed once it has been silent for `PAPERJAM_EXTRACT_TIMEOUT_SECS`. That is the wait for the text of the whole PDF, for each rendered page, or for the decoded image.
- **Memory.** On Linux the server caps the worker's address space at `PAPERJAM_EXTRACT_MEMORY_MB` before the worker starts, so the cap also covers what the allocator reserves at start-up. Going past the cap makes an allocation fail and ends the worker. Other systems run it without a limit.
- **Confinement.** It starts with an empty environment, so it has neither the secret key nor the API key. It dies with the server (`PR_SET_PDEATHSIG`), writes no core dump and cannot gain privileges. It needs no file system access beyond its own binary, so the container's read-only root and `cap_drop: ALL` are enough.
- **Errors.** The codes a worker reports are matched against a fixed list. A worker that dies, times out or breaks the protocol is reported as a corrupt file. One that cannot be started at all is `worker_unavailable`, an internal error, so the job is retried.

OCR is Tesseract in a process of its own already. What remains in the server is work on pixels it was handed: scaling, deskewing and WebP encoding.

## Search

tantivy indexes the title, every text layer (losing layers are searchable but not displayed), the summary, type, correspondent, people with their aliases, tags and field values. One analyzer per configured language (English always, plus the stemmed languages in `PAPERJAM_OCR_LANGS`) lowercases, folds to ASCII and stems, each into its own field, and a plain lowercase field serves exact identifiers. Typed field values live in a JSON field keyed by field key, so adding a field needs no schema change. The query language is described in [MCP: search syntax](mcp.md#search-syntax).

The index is derived data. Its writer is created lazily, commits every two seconds while there is something to commit, and is dropped after a minute of idleness to keep idle memory low. A search commits pending changes first, so it sees everything written before it. An index error never fails a user's request.

The text shown for a page comes from the best available layer: good embedded text (exact) first, then `llm`, then `tesseract`, then poor embedded text.

## Links, feeds and sharing

Four kinds of link exist, with different secrets:

- A **download link** (`/api/v1/dl/<token>`) is signed with the secret key and valid for 10 minutes. It serves the original.
- A **one-time upload link** (`/api/v1/ul/<token>`) holds a random token, stored hashed, valid for 15 minutes and for one file. It is how a large file reaches the server without passing through a chat.
- A **share link** (`/s/<token>`) shows a document to anyone who holds it, for 15 minutes, an hour or a day. The token is the share's id plus a signature of it, so the database holds no secret, and the link can be shown again while it is live. People list and revoke live links in the web app.
- The **calendar feed** (`/calendar/<secret>.ics`) has a secret that is stored hashed and shown once. It can be rotated or revoked.
- The **inbound email webhook** (`POST /api/v1/ingest/email`) accepts raw RFC 5322 mail authenticated with the write-only `PAPERJAM_EMAIL_SECRET` and optional sender allowlist (`PAPERJAM_EMAIL_ALLOWED_SENDERS`).

## Operations

- **Backup** writes a `VACUUM INTO` snapshot of the database, which is consistent while the server runs, hard-links or copies the originals, and writes a `manifest.json` with hashes. Derived files and the index are not included. Restore checks every original against the manifest before it writes anything, and refuses a data directory that already holds data unless told to replace it.
- **Export** is a zip of the originals plus open JSON and text metadata (`documents/<id>/original.<ext>`, `metadata.json`, one text file per page and source, and `taxonomy.json`), documented in its own `README.txt`. It is the guarantee against lock-in.
- **Health.** `GET /healthz` checks the database. `paperjam healthcheck` queries it and is the container's `HEALTHCHECK`.
- **Identifier checks** are described in [validators](validators.md).
