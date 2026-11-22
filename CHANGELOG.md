# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres
to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Security
- The Cloudflare Email Worker in `docs/email-intake.md` parses `From:` the way paperjam does, nested comments included. Its one-pass comment stripping let `attacker@evil.example (x (y) <alice@example.org>)` pass an allowlist for `alice@example.org` while paperjam saw the attacker. The guide now says to set `PAPERJAM_EMAIL_ALLOWED_SENDERS` on the server as well, the server warns at startup when it is unset, and a present but empty or non-ASCII `X-Envelope-From` is refused instead of falling back to `From:`.
- A PDF with a huge MediaBox no longer exhausts memory: a page render is capped at 16 Mpx (A4 at 300 DPI is 8.7 Mpx), and pages waiting for OCR are kept at the 3508 px OCR reads instead of full size.
- The model can no longer create a correspondent whose name the document does not print, so a hostile document cannot plant an instruction-like name that every later classification prompt would carry. Names, aliases, type labels and tags reach the prompts as JSON strings marked as data.
- Sign-in: password failures are also counted per client address over all usernames, so random names from one address cannot keep every password check busy; a password check gives up after 10 s waiting for a free slot instead of queueing without end; IPv6 clients count by their /64 in every limiter (sign-in, passkey challenges, OAuth client registration); a full limiter evicts its oldest entries and keeps live lockouts instead of forgetting everything.
- The service worker no longer caches shared-document pages, OAuth screens, calendar feeds or any response marked `no-store` or `private`, and stores only the app shell from navigations; the cache version is bumped so older caches are dropped. Signing out clears the cached pages, and clears local data even when the server cannot be reached.
- Each LLM call sets an estimate of its cost aside before it is sent, so calls running at once (pages transcribed side by side, several documents) can no longer all pass the monthly budget check and overspend it.
- A passkey whose signature counter counted before and now sends 0 is refused as a possible clone; synced passkeys that never count still work.
- Moving a document to the trash revokes its download links at once. `get_download_link` and `share_document` are marked not read-only and open-world, so MCP clients ask before minting a URL anyone can open.

## [0.2.0] - 2026-11-21

### Added
- Inbound email webhook (`POST /api/v1/ingest/email`) gated by a write-only bearer secret (`PAPERJAM_EMAIL_SECRET`), rate limiting and an optional sender allowlist (`PAPERJAM_EMAIL_ALLOWED_SENDERS`). Extracts supported attachments (PDFs, images including iPhone inline photos, explicit `.txt`/`.md`, and nested `message/rfc822` forwards) while skipping embedded signature assets, tracking pixels and non-document parts (`.ics`, `.vcf`, `.p7s`, `winmail.dat`). Falls back to converting attachment-less email bodies into `.md` when `PAPERJAM_EMAIL_BODY_FALLBACK=true` (default), with optional `PAPERJAM_EMAIL_USER`, `PAPERJAM_EMAIL_LOCAL_ONLY` and `PAPERJAM_EMAIL_TAG` settings, plus a Cloudflare Email Worker guide in `docs/email-intake.md`.
- A built-in `power_of_attorney` type (Доверенность): principal, representatives, powers, issue date, term, expiry date, notary (the issuer field), registry and form numbers, right of substitution. It is due on its printed `expiry_date`, or a printed term after its `issue_date` ("сроком на десять лет"). Existing archives get the type at the next start; documents filed as Other move to it on a reprocess.
- Handwritten pages get a second reading: the vision model reports pages whose values are written by hand, and `PAPERJAM_LLM_HANDWRITING_MODEL` (on OpenRouter `anthropic/claude-sonnet-5.5` by default, `off` to disable) transcribes them again. The default vision model misread cursive names, dates and personal numbers on filled-in birth certificates.
- A page whose transcription could not read every word flags the document as `illegible`: the text keeps the model's `[illegible]` markers, the flag says to compare the scan, a clean re-read on a reprocess clears it, and dismissing it says the reader checked.

### Changed
- Pipeline throughput and latency optimizations:
  - PDF rendering in the extraction worker runs in parallel across up to 4 threads (bounded by core count), caps scan OCR rendering at native scan density (`>= 150` DPI floor), reuses frame buffers zero-copy across IPC, passes raw binary PNM to Tesseract with single-pass TSV parsing, uses coarse-to-fine skew estimation, streams clean PDF assembly, avoids redundant WebP/thumbnail encodes, and uses faster WebP compression on ordinary pages (falling back to stronger compression only on the final size-budget step); local PDF extraction scales page concurrency up to 4 per worker based on available CPU cores.
  - OpenRouter requests default `reasoning.effort` to `none` for page transcription (including `transcribe_handwriting`) and field extraction (keeping `low` for classification), automatically falling back to `low` if a model returns HTTP 400 and caching that model's capability in memory. Multi-page scans now transcribe 1 page per request up to 6 in flight, trigger handwriting re-reads immediately per page while keeping each page's own readability verdict, cache the transcription JSON schema and validator, omit count subqueries from classification taxonomy loading, add ephemeral prompt caching for Anthropic system prompts over 1 KiB on OpenRouter, and run classification and field extraction concurrently when a document type is locked (`manual` or `mrz`).
  - Document processing commits fast page previews, thumbnails, and embedded text right away while remaining in `processing` (no premature `ready` flicker), then overlaps LLM transcription with background Tesseract OCR and clean PDF assembly (storing local OCR/MRZ results safely without clobbering concurrent LLM output and coordinating short docs and fallback paths cleanly).
  - Read-time field checks skip unflagged OCR/checksum/MRZ scans on clean `ready` documents and precompute page digits once; `update_document` refreshes attention checks on type changes; search indexing constructs `TantivyDocument` values directly without JSON roundtrips; page image route existence checks are async; and an index on `events (action)` speeds up event lookups.
- Image deskew rotation now uses fixed-point incremental sampling, and unsearched PDF pages skip downscaled search-copy generation.
- The transcription prompt forbids the classic handwriting failure — replacing an unreadable word with a plausible street, city or name — and the field prompt treats `[illegible]` markers as unstated; a value that only says it cannot be read (`[illegible]`, `неразборчиво`, `N/A`) never becomes a field or a fact.
- Classification must give a document the listed tag of its domain when one fits (identity, finance, housing, health, veterinary); an empty tag list is only for a document no listed tag fits, and `new_tags` is proposed when the domain has no tag yet.
- Web UI on phones: a tap on an archive card starts the motion at once and the document opens on its loading state instead of waiting up to 250 ms for its data and first page image; going back, or tapping a filter chip, on an archive that is already on screen no longer waits for a fresh search. Screen transitions are shorter (360–380 ms, close to the system's own), the card-to-page flight is a pure transform, and on touch devices the documents of the cards in view are prefetched (text only) in idle time. Hidden screens are no longer redrawn on every navigation step, a page learns it is on top only once its motion is over, archive rows are memoized, date and size formatters are built once per language, and a returning screen no longer forces a layout to restore a scroll position of 0.

### Fixed
- Web UI on phones: tapping the top bar's back button no longer leaves the screen stuck while the address goes back (a tap near the left edge started an edge swipe whose cancel swallowed the navigation); the edge swipe now starts only after a horizontal move and only in the installed app, leaving the browser's own back gesture alone in a tab. The whole archive card opens its document (or ticks it while selecting), its chips keep a 44 px touch area, and a tap no longer lights a card or person up as keyboard-selected or raises the on-screen keyboard after a chip. Form fields are 16 px on touch screens so iOS does not zoom on focus, small buttons, dropdowns and the local-only switch get 44 px touch targets, the tap highlight flash is gone, the upload button scrolls into view above the bottom bar once files are picked, the progress stays in view after starting, picked images show a thumbnail, and the filter help no longer tells a phone to press Enter.
- Classification no longer takes the office that issued a person's identity document, copied into a deed or a contract ("паспорт …, выданный МВД по …"), for the document's own issuer; a notarized document is issued by its notary. A power of attorney listing its representatives' passports was linked to the office that issued one of them.
- `due_date` is read per type: a rental agreement, contract or insurance policy is due on its `end_date`, a warranty on `warranty_end_date`, an invoice on its `due_date`, identity documents and certificates on their `expiry_date`, and a custom type on the first of those fields it defines and fills. Previously only `expiry_date` counted, so a lease about to run out stayed out of `has:expiring`, the calendar and the document's due chip. The search index version is bumped, so a start after the update re-derives every stored hit's due date from its fields without calling any model.
- People relinking matches romanizations the fixed table does not produce (`Artiom` for `Артём`, `Iurii` for `Юрий`) and names that differ only by a patronymic the document does not print, so a Cyrillic person is found by a Latin document with no stored alias. The relink job also matches the values already extracted into fields and facts (a birth certificate's `father`, a lease's `permitted_resident`, the spouses of a marriage) and runs once at every start, so a person created after a document was uploaded attaches to it without a reprocess, a manual relink or an LLM call.
- Organizations whose names differ only in the designation word ("Отдел ЗАГС …" / "Управление ЗАГС …") no longer become two correspondents: creation reuses the stored one, and the startup tidy merges such pairs, keeping both spellings as aliases. Digit runs of one code fold together (`МВД 123-045`, `МВД 123 045` and `МВД 123045` are one office).
- A person the model links to a document must be named in its text (every word but a patronymic, any case ending): a couple's marriage certificate no longer gets an unrelated listed person.
- Organizations: the model's answer now replaces a local name match even when it names no organization, so an authority printed on an attached passport copy no longer becomes an organization of a lease. A picked correspondent needs at least half of its words in the text, and a numbered office the model read ("МВД 10001") never backs a stored name without that number.
- `ocr_llm_disagree` fires only when Tesseract printed a near miss of the value (a number a few digits off). A value it printed nothing like, as on decorative certificates whose OCR is noise, is no longer flagged.
- MRZ name checks compare Cyrillic in the ICAO spelling a passport zone uses (Я = IA, Х = KH, Ц = TS, Щ = SHCH), so `ЯКОВЛЕВА`-style names no longer raise `mrz_mismatch` against their zone.
- The MCP output schema of `get_document` and `update_document` now declares the `label` of facts; documents with labelled facts failed client-side validation.
- Turn search and full-page OCR now fall back to sparse page segmentation (`--psm 11`) when default segmentation (`--psm 3`) drops valid text on sparse or photo-heavy spreads, and ambiguous turn-search fallbacks stay at the upright scan orientation instead of applying a low-confidence OSD flip.
- Identity documents now guard against matching a relative who shares only a surname or patronymic: classification prompts explicitly require a full-holder match, mismatched non-manual links are pruned once `surname`/`given_names` are extracted so the actual holder can be linked, and auto-generated titles referencing a mismatched person are updated to the extracted holder.

## [0.1.0] - 2026-11-14

### Added
- Zero-configuration first start: the secret key is generated into the data
  directory (and carried by backup and restore), and the first administrator
  is created on the web page with a one-time setup code that the server prints
  to its log. `PAPERJAM_ADMIN_USER` and `PAPERJAM_ADMIN_PASSWORD` create it at
  start-up instead; `paperjam user add` still works.
- `install.sh`, an interactive installer that writes the compose file and
  `.env`, starts paperjam and prints the setup code. The compose file gained an
  optional bundled Caddy (`COMPOSE_PROFILES=https`) for HTTPS on a domain and
  `PAPERJAM_LISTEN` for opening the port to a home network.
- Checksum validators for IBAN and for national numbers of France (NIR),
  Germany (tax ID), Spain (DNI/NIE), Poland (PESEL), the Netherlands (BSN),
  the UK (NHS number), Brazil (CPF) and India (Aadhaar), usable by name on
  custom document types. `personal_number` now also recognizes the French and
  Spanish shapes. The bank statement's account number is checked as an IBAN
  when it has the IBAN shape.
- The container image ships Tesseract packs for 12 languages and takes
  `--build-arg OCR_PACKS=...` for others.
- Search stemmers for 18 languages, chosen by `PAPERJAM_OCR_LANGS`.
- The built-in document types are also found by their everyday German,
  French, Spanish, Italian and Polish names (`Führerschein`, `permis de
  conduire`, `permiso de residencia`, `patente`, `dowód osobisty`, ...).

### Changed
- An `http://` `PAPERJAM_PUBLIC_URL` now gives the session cookie that works over
  plain HTTP, even when the server listens beyond loopback (it used to be
  `Secure` there, which locked out a home network without TLS).
- `PAPERJAM_OCR_LANGS` now defaults to `eng` (was `eng+rus+ron`). Set it to the
  languages of your documents; it also picks the search stemmers, and a change
  rebuilds the search index at the next start. Installs that relied on the old
  default must set `eng+rus+ron` explicitly.
- The built-in document types are named for an international audience: the
  travel passport is "Passport" and the series-bearing one is "Internal
  Passport" (English labels of new installs only).
- The README is rewritten for someone installing paperjam for the first time:
  install and first run, connecting an agent, what it does, privacy, and a
  comparison with Paperless-ngx, Papra and Readur dated to when it was
  checked. Backup and restore, the Paperless-ngx import and the calendar feed
  moved to `docs/configuration.md`.

### Fixed
- `docs/configuration.md` listed four OCR language packs (`nld`, `ces`, `swe`,
  `tur`) that the container image no longer ships.

### Security
- Request bodies are capped at 64 KB everywhere except document upload, the
  one-time upload link and MCP, which keep their larger limits. Before, an
  anonymous sign-in with a megabyte-long username was held in memory (and its
  name in the rate limiter) until the next restart. Sign-in now refuses a
  username over 64 characters or a password over 1024 bytes before the
  database and before the failure counter, and at most three Argon2 checks
  run at once, so a flood of sign-ins queues instead of eating the host's
  memory and cores.
- Creating users and issuing API tokens needs a signed-in web session (the
  server CLI still works). Before, an administrator's token could mint a new
  administrator or a spare token, reach web-only actions such as permanent
  deletion and outlive its own revocation.
- OAuth client registration: a redirect URI is at most 2048 characters, the
  registration body at most 16 KB, and only HTTPS, localhost and the deep
  links of known desktop clients (Claude, Cursor, VS Code, Windsurf, Zed,
  Goose, Raycast) are accepted. For an application nobody has approved yet,
  errors in the authorization request (wrong `response_type`, missing PKCE)
  show a page on paperjam instead of redirecting the browser to the address it
  registered. The consent page shows the whole return address and says when
  the application registered itself.
- Sign-in limits count per real client address and username. Behind a reverse
  proxy the address comes from a header that only trusted proxies
  (`PAPERJAM_TRUSTED_PROXIES`, `PAPERJAM_CLIENT_IP_HEADER`) may set, so
  failures from one place no longer lock everyone out. Whitespace and case in
  the username no longer start a fresh count. After five failures each further
  one doubles the wait (up to 15 minutes) instead of a hard block; past a
  hundred failures an hour on one name from anywhere, password attempts on it
  are spaced ten seconds apart while passkeys keep working. Passkey sign-ins
  are limited per client address, and a flood of sign-in challenges no longer
  blocks adding a passkey.
- The session cookie over HTTPS is `__Host-pj_session`, and a request that
  changes something with that cookie is refused unless it comes from
  paperjam's own pages. Pages on a neighbouring subdomain can no longer upload
  or trash documents in a signed-in browser. Everybody is signed out once.
- Responses add `base-uri 'none'` and `form-action 'self'` to the content
  security policy, plus `Cross-Origin-Opener-Policy` and `Permissions-Policy`.
- `paperjam restore` checks every storage key in the backup manifest with the
  same rule the file store uses and refuses a backup with a path that would
  escape the data directory, before anything is copied.
- Release builds unwind again, so a panic in a job handler fails that job
  instead of the whole server.
- PDFs and images (HEIC included) are parsed in a child process
  (`paperjam extract-worker`), never inside the server. A parser that hangs,
  blows up on a decompression bomb or corrupts memory ends only that process:
  the document fails with `pdf_corrupt` / `image_corrupt` and the server keeps
  serving. The child is killed after `PAPERJAM_EXTRACT_TIMEOUT_SECS` (default
  120) of silence, is capped at `PAPERJAM_EXTRACT_MEMORY_MB` (default 1536,
  Linux), starts with an empty environment (no secret key or API key), dies
  with the server and can write no core dump. Cost: one process start per
  document, 0 to 40 ms in the local benchmark (best of five runs, seven
  fixtures, from a one-page text PDF to a multi-page statement); the rendered pages
  cross a pipe as raw pixels.
- The server shuts down gracefully on SIGTERM (`docker stop`), not only on
  Ctrl-C.
- Reprocessing accepts only the models in `PAPERJAM_LLM_ALLOWED_MODELS`
  (default: the configured ones), and only a web session can clear
  `local_only`, so a token or an agent misled by a document cannot send a
  local-only document to the cloud or pick an expensive model.
- CI runs with a read-only token; the compose example limits memory and
  processes, drops capabilities and mounts the root file system read-only.
- The consent page for connecting an application says, in English and Russian,
  that the application can also create public share links to documents and
  start reprocessing, which may send a document's text to the configured LLM
  unless the document is local-only.
- Expired web sessions are deleted from the database when someone signs in and
  at start-up. Before, they were refused but never removed, so the table only
  grew.
- Published images are signed with keyless cosign and carry build provenance
  and an SBOM per platform as attestations; `docs/releasing.md` shows how to
  verify them.

### Added
- Account lifecycle and password management:
  - Any signed-in web user can change their own password in Settings → Access
    (`POST /api/v1/auth/password`, web session only), keeping the current
    session alive and signing out other sessions.
  - Administrators can disable, re-enable, reset passwords for, and permanently
    delete other accounts from Settings → Access (`PATCH` and `DELETE
    /api/v1/users/{id}`, web session only), and via `paperjam user disable`,
    `enable`, `delete`, and `passwd`. Deleting an account removes its sessions,
    API tokens, passkeys, OAuth grants and calendar feed while keeping archive
    documents intact; the active administrator cannot disable, demote or delete
    themselves or the last active administrator.
- The LLM layer works with any OpenAI-compatible chat-completions endpoint
  (OpenAI, Gemini's compatible endpoint, Ollama, vLLM, proxies), not only
  OpenRouter. The profile follows the host of `PAPERJAM_LLM_BASE_URL`
  (`openrouter.ai` keeps today's requests exactly; any other host gets standard
  fields only) and `PAPERJAM_LLM_PROVIDER` overrides it. For endpoints that
  report no cost, `PAPERJAM_LLM_PRICES` prices calls per model so the monthly
  budget keeps working; without a price the cost is recorded as zero, one
  warning is logged and the budget cannot protect that setup. Quality is only
  guaranteed on the default model.
- Release packaging: a workflow publishes a multi-arch (amd64, arm64) image to
  `ghcr.io/ilodezis/paperjam` on version tags and on `main` (`edge`), built on
  native runners, and reports image size and idle memory. The compose example
  now pulls that image. See `docs/releasing.md`.
- Calendar subscription. Settings → Access makes a private
  `/calendar/<secret>.ics` link that Apple, Google and Outlook calendars poll:
  one all-day event per document on its due date and a "renew" event
  `PAPERJAM_CALENDAR_LEAD_DAYS` days earlier (default 60, `0` for none), worded
  in the user's interface language. The feed holds titles and dates only, plus
  a link to the document when `PAPERJAM_PUBLIC_URL` is set, never field values,
  and skips trashed documents and those marked *Never send to the cloud*. The
  secret is separate from API tokens, opens only the feed, is stored hashed and
  can be created, rotated and revoked (`GET`, `POST`, `DELETE`
  `/api/v1/calendar-feed`). Adds migration 0010.
- Inbox folder. With `PAPERJAM_INBOX_DIR` set, paperjam polls that directory
  (subfolders included) and adds every file that has stopped changing through
  the same path as a web upload, then deletes the source. Duplicates are
  removed, rejected files move to `.failed/` with a reason file.
  `PAPERJAM_INBOX_USER` picks the account (optional with a single user) and
  `PAPERJAM_INBOX_INTERVAL_SECS` the scan interval (default 30).
- `paperjam import paperless <export-dir>` brings in a Paperless-ngx
  `document_exporter` export (plain, `--split-manifest` or
  `--use-filename-format`): the original files through the normal upload path,
  with title, created date, correspondent and tags kept as manual values and the
  document type as a `paperless:<type>` tag. `--dry-run` reports without
  writing; duplicates are skipped, failures are listed per document, and a
  re-run is safe.
- iOS Shortcut guide ([docs/ios-shortcut.md](docs/ios-shortcut.md)) for sharing
  files from the iOS share sheet to paperjam over the REST API with a personal
  token, with a local-only variant, troubleshooting and the token revoke
  command. The README's iOS notes point to it.
- Expiring documents. A document has a due date: its `expiry_date` field, or,
  for a Russian internal passport (which prints none), the 20th or 45th
  birthday it has to be replaced at (Government Decree No. 828; issued before
  20 → the 20th, between 20 and 45 → the 45th, at 45 or later → none). The
  archive offers an "Expiring soon" quick filter when something is due within
  270 days or past due, cards and the document page show the due date, and the
  query language has `has:expiring` (relative to the day of the query),
  `has:due` and `due:<2027-06`, the same over REST and `search_documents`. Hits
  and `get_document` carry `due_date`. The search index is rebuilt on the
  first start after the update.
- "Create “…”" in the People and Tags pickers on the document page: typing a
  name nobody has offers to create the person or tag and put it on the
  document in one step (Enter does it from the keyboard).
- MCP upload widget. In clients that render MCP apps (Claude.ai), calling
  `create_upload_link` shows a file picker in the chat; the browser uploads the
  chosen scans straight to their one-time links, so file bytes do not pass
  through the chat or the model. A "keep away from the LLM" checkbox sets
  `local_only`. Other clients get the same tool result as before. The upload
  route answers CORS from any origin (the token in the path is the credential).
- A document can have several organizations, each with its own role in that
  document: issued by, received from, contract party, employer, or no role
  stated. Existing links keep working and start with no role stated; nothing
  is guessed from the name. The document page edits the list, its heading
  follows the roles (or the document type), and the archive shows a chip per
  organization. The REST API, the `update_document` tool, search hits and the
  export carry `organizations` in place of the single `correspondent`; the
  directory endpoints are unchanged. The search index is rebuilt on the first
  start after the update.
- Share links. "Share" on a document makes a link to a page at `/s/<token>`
  that shows the document to anyone holding it, without signing in: its pages,
  a download button and, if chosen, the summary and extracted values. A link
  works for 15 minutes (the default), an hour or a day; the document's live
  links are listed in the share sheet to copy, send or revoke. A revoked or
  expired link shows a page saying so. The page sends no preview image, so a
  messenger shows only the title. REST: `POST`/`GET /documents/{id}/shares`,
  `DELETE /shares/{id}`; MCP: `share_document`.
- Clean copies of scans. Each page of a scanned document is turned upright,
  cut out of the sheet, straightened and trimmed, and the pages are bound into
  one PDF. "Download" serves this copy; the uploaded file stays available as
  "Download original file". Built for images and for scanned PDFs where
  cleaning changed a page; born-digital PDFs are served as they are.
  Perspective is not corrected, so a photo taken at an angle stays at that
  angle. Documents processed before this version get a clean copy when they
  are reprocessed.

### Changed
- Search hits carry `attention`, the document's attention flags. The search
  index schema moved to version 5, so the index is rebuilt once on startup.
- The local-only setting has one name everywhere: "Process on this server
  only".
- `PAPERJAM_LLM_API_KEY` and `PAPERJAM_LLM_BASE_URL` replace the OpenRouter
  names. `PAPERJAM_OPENROUTER_API_KEY` and `PAPERJAM_OPENROUTER_BASE_URL` keep
  working; the new name wins when both are set.
- Errors and logs say "The LLM API" instead of "OpenRouter" for endpoints
  other than OpenRouter, and the MCP reprocess `model` parameter no longer
  calls its value an OpenRouter model.
- `PAPERJAM_LLM_REASONING_EFFORT` has no built-in default for endpoints other
  than OpenRouter (which still gets `low`): the field is sent only when set.
- Local processing of scans is about three times faster. On a two-page
  phone-scanned passport it took 16.8 s and now takes about 5 s on the same
  machine: the first page, which orientation detection had turned upside down,
  went from 16.7 s to 5 s. Among the first two pages, a page whose orientation
  verdict is unsure (an ID card, a passport page) has its turn found on a
  1200-pixel copy before it is read, and read in full once; the first reading
  also tells whether the page lies sideways (its words stand on end), so at
  most three turns are tried instead of four full-size readings and a second
  full read. A page of text with a sure verdict is no longer read a second time
  to check its turn. The page render, the clean page's render and its JPEG are
  encoded side by side, a page that cleaning left unchanged is encoded once for
  both renders, and deskewing turns a 300 DPI page in about half the time. Each
  page logs where its time went (`page processed`: render, orientation, turn
  search, reading, MRZ pass, straightening, encoding), and every Tesseract run
  is logged at debug level.
- Licensed under the GNU AGPL-3.0-or-later in place of MIT. Outside contributions
  come under the CLA in `CONTRIBUTING.md`, so the maintainer can also offer a
  paid hosted service.
- The clean copy of a scanned PDF keeps the scan's own pixel density instead
  of the 300 DPI it is read at: a 150 DPI scan gave a page of four times the
  pixels and no more detail, so the copy was larger than the original (on the
  synthetic 150 DPI invoice scan, a clean page of 219 KB is now 79 KB). OCR
  still reads the 300 DPI render. Cropping and straightening are unchanged and
  happen before the downscale; photos and image uploads, already at their own
  density, are not touched. Documents processed earlier keep their larger
  copy until they are processed locally again.
- The web interface ships only the font subsets it uses (Latin, Latin
  Extended, Cyrillic, Cyrillic Extended) and only as WOFF2: 34 `.woff` fallbacks
  and the Greek and Vietnamese subsets are gone. The text assets (JS, CSS, HTML,
  SVG) are embedded in the binary gzipped and sent as they are to clients that
  accept gzip (`Content-Encoding: gzip`, `Vary: Accept-Encoding`); other clients
  get them inflated. Embedded web assets shrink from 1.50 MB to 0.55 MB.
- A PDF is refused above 300 pages (was 2000): a run holds every page's
  results in memory until the document is stored, so a very long scan could
  exhaust the container's memory.
- Downloaded files are named after the document's title ("Passport — Ivanov
  I. I.pdf") instead of the uploaded file name. The line above the title on
  the document page shows the file's format and size.
- The 10-minute download link panel is replaced by the share sheet.
- Web interface polish. Archive thumbnails show the whole page (a landscape
  card is no longer cut in half) and fade in instead of popping. Changing a
  filter keeps the current list on screen, dimmed, until the new one arrives.
  A document's description is shown in full (long ones fold to a few lines)
  and is edited in place. "Never send to the cloud" is a switch. Every select
  uses the same custom dropdown; checkboxes and the segmented controls are
  redrawn without the doubled outlines. The Directories section is now
  "People & organizations", and its other names, merging and deletion are
  always in view instead of behind disclosures. Tab panels, record switches
  and dropdowns share one short entrance motion.
- Titles written by the model no longer start with the document type: the
  interface already shows it next to the title, and "Receipt — 2026" told
  nothing when there are a hundred receipts. A title now says what sets the
  document apart: shop, amount and date for receipts and payments, holder and
  year for identity documents, parties or subject for contracts, a short
  subject otherwise. A type label the model still writes in front is removed.
  Existing documents get the new titles when they are reprocessed.
- Lower idle cost. Job workers sleep until they are woken by a new job or until
  the next retry is due (at most 60 s) instead of polling the database every
  second each. The index committer no longer borrows a blocking thread every
  two seconds when there is nothing to commit. The allocator and huge-page
  settings are applied before the async runtime starts its threads, and the
  search writer uses one merge thread instead of four.
- Smaller container image: the runtime installs the Tesseract library and the
  language packs and takes only the `tesseract` binary from Debian's
  `tesseract-ocr` package, leaving out its training tools and the ICU, GLib,
  Cairo, Pango, X11 and font libraries they pull in (about 58 MB). The binary
  is linked with packed relative relocations and built as one codegen unit
  (about 5 MB smaller). CI now OCRs a fixture inside the built image on both
  architectures before it is tagged.
- The footprint report in CI uploads the synthetic corpus through the API and
  measures memory and threads at idle, after processing and at the peak,
  instead of reading `docker stats` ten seconds after start-up.

### Fixed
- Reprocessing from a document page now confirms queueing with a toast and
  immediately replaces the attention badge in the status line with the queued or
  processing indicator and spinner while the job runs.
- `mrz_mismatch` now points to the specific field(s) that disagreed with the
  machine-readable zone, clears automatically once those fields are confirmed or
  edited manually, and can also be dismissed directly from the attention list
  (`dismiss_attention` in `PATCH /api/v1/documents/{id}`, alongside carried
  processing flags and scan-level MRZ checksum failures). Adds migration 0011.
- A machine-readable zone in the model's transcription whose check digits
  verify now replaces the local OCR reading even when local OCR already read
  valid check digits: a misread names line (which carries no check digit) no
  longer raises `mrz_mismatch` or blocks linking the holder when the model
  copied the zone cleanly. Manual fields stay untouched.
- The upload result list shows the extracted title with the file name beneath
  it, and one labelled icon link per row instead of an identical "Open
  document" button on every row.
- An unknown address no longer highlights any navigation item.
- A failed or text-only document no longer requests a page image or
  thumbnail that does not exist, in the archive list or on its page, so the
  console shows no 404s for them. Search hits and the document's `file` carry
  a new `has_pages` flag (also in the MCP results) for this; the search index
  is rebuilt once on the first start to fill it.
- Trash rows show the document's thumbnail instead of a generic icon.
- The page declares the standard `mobile-web-app-capable` meta next to the
  deprecated Apple one.
- A Russian internal passport whose zone carries the patronymic and whose
  printed given name does not no longer raises `mrz_mismatch`: the zone name
  is kept and the holder becomes a person.
- The year at the end of an identity document's title is always the year of
  issue (the `issue_date` field, else the document date), never the expiry.
- A person created from a document holder takes the spelling in the user's
  script once a document prints it (Cyrillic for a Russian-language user);
  the earlier spelling stays as an alias, so search by either finds them. A
  person the user renamed or merged is left as it is.
- A holder name printed in two scripts ("TESTOVA / ТЕСТОВА") becomes a person.
- An upload processed in two jobs (recognition, then the LLM steps) no
  longer turns a holder read only from the machine-readable zone into a
  person before the model has read the page: a misread zone name ended up
  as a person even when the page contradicted it.
- `field.number=123456` and other all-digit values now match identifier
  fields, which are stored as text.
- An issuer field that names an organization already in the directory links
  it even when the model calls the value a place; a mere place is still
  never linked.
- The model is told that a certificate number is the document's own printed
  number with its series, never a personal number on it.
- Calendar events carry the document type before the title, so two
  documents with the same title can be told apart.
- Pressing Escape on a document page with a menu open closes the menu
  instead of leaving the page.
- An unknown address shows a "page not found" screen instead of the archive.
- A failed document explains its failure in words instead of a raw code and
  shows no page controls; one-page documents no longer label every field
  with "p. 1".
- Leaving selection mode when nothing is left to select; an empty archive
  shows only its empty state.
- Upload: compact per-file privacy selector, localized file sizes, the stage
  and its count said once, no in-progress button after the files are sent.
- The archive list marks documents that need attention, and the status on a
  document page jumps to the attention notes.
- The share sheet has a close button; the API token list refreshes its
  "last used" time when the tab opens; the calendar lead time and the LLM
  spend read naturally.
- A machine-readable zone read upside down (capitals and a stray `<`) no
  longer counts as a zone when the turn of a page is chosen: a zone line now
  needs a run of `<` filler or the digits of its numbers and dates.
- An OCR misread in the names line of a machine-readable zone no longer
  overrides the printed name or turns into a person. Names carry no check
  digit, so the zone's spelling is kept only when it matches the printed
  reading exactly (Russian internal coding, or ICAO and the older Russian
  transliteration for travel documents); a zone name that differs only by a
  usual OCR confusion (`8`/`B`, `0`/`O`, filler read as `X`) is replaced by the
  printed one, any other difference also raises `mrz_mismatch`. A names line
  that lost its `<<` separator, or decodes to an impossible Russian name (a
  one-letter word, `Ъ`/`Ь` after a vowel), brings no names at all instead of
  putting the given names into the surname; a speck before `PNRUS` and digits
  inside ICAO names are repaired. The holder of an identity document becomes a
  new person only at the end of a run, after the model checked the names, and
  never while `mrz_mismatch` is set.
- The search index writer (about 4 MB of tables and six threads) was kept
  until the next restart when a change was followed by a search within two
  seconds; it is now released after a minute without use, whoever committed.
- Identity-document titles no longer compute an artificial expiry year when none
  is printed (e.g. Russian internal passports): classification instructions use
  the year of issue, and expiry only when an expiry date is printed.
- Stored organization names are tidied on startup (`tidy_correspondents`): quote
  spacing is normalized, duplicate records are merged, and bare abbreviations
  (e.g. `ASP`) are merged into full parenthesized names (`Agenția Servicii Publice (ASP)`).
  The classification candidates prompt explicitly forbids selecting bare place names.
- People created from Cyrillic identity documents with a patronymic are named
  "<Surname> <Given> <Patronymic>" with "<Given> <Surname>" as an alias, and
  matching finds existing people by short or full name.
- The holder of an identity document is now a person. A passport, ID card,
  travel passport, driver's licence or residence permit with a name that
  matches nobody in the archive creates the person (given names then surname,
  not in capitals, in the script of the uploader's interface language, with the
  other spelling as an alias) and links the document; the same holder in the
  other script finds that person instead of adding a duplicate. Names on other
  documents (a birth certificate's parents, signatories) never create people,
  and people set by hand are left alone. A reprocess and the relink job apply
  the same rule, so existing documents are brought along.
- Tags no longer stay empty: `PAPERJAM_LLM_CREATE_TAGS` is on by default. A
  document gets at most two new tags, short topic words in the output
  language; an existing tag of the same name (any letter case, stray spaces)
  is reused, and a tag that repeats the document type, a person or an
  organization is dropped. Set it to `false` for the old behaviour.
- A born-digital PDF whose issuer is only a logo (an insurer's letterhead on
  page 1) no longer ends up with no organization: classification now also
  sees a small image of page 1, next to the text. Scanned documents and
  documents marked "never send to the cloud" are unchanged.
- Summaries no longer carry the model's extraction notes ("the date is not
  stated", "page 64 onwards is present but the issuer is unreadable").
- Organization names are tidied before they are matched or stored: stray
  spaces inside quotes (`ООО " Ромашка"`) are removed, typographic quotes
  become plain ones, and a name that differs from a stored one only in such
  spacing is no longer added as a second organization. The model is asked for
  the full official name (the abbreviation in parentheses when the document
  prints only that) and to leave out a city that is not an organization.
- A scan with a clean copy is now shown the way the copy shows it: the page
  images of the document page, the thumbnails and shared pages are the clean
  page (upright, cut out, straightened), not the tilted original. The viewer has
  a toggle for the pages as they were read. `paperjam rerender` brings
  documents processed earlier along, from their clean copy; no reprocess, and
  the old renders are kept as the originals.
- After uploading, the settings' list of document types and the people and
  organizations did not show what the new document brought until the next
  visit: the cached taxonomy and counts lived ten minutes on the device and
  nothing refreshed them when a document finished processing. They now live
  half a minute, and every screen that sees a document settle, the upload page
  included, refreshes the archive's lists, taxonomy, counts and trash together;
  the same goes for edits made on the people and organizations screen.
- `field.expiry_date<2037` found nothing: a bare year was read as a number. On
  a field that holds dates a bare year is that year (`<` before it, `=` any day
  of it, `>=` from it on); number fields are unchanged.
- A search for several words that no single document holds, such as "паспорт
  мамы", ends in an empty list. It is now repeated with the words joined by
  `OR`; the answer says `partial: true` (REST and `search_documents`) and the
  archive page says so above the list. Queries with a number in them, an
  explicit `OR`, or a single word are not relaxed. The empty archive offers
  "Clear all filters" only when the query has a filter in it.
- The OpenAPI document describes the `POST /search` request body (query,
  filters, sort, limit, cursor) and its answer.
- Copy: empty people and organization lists read "Не указаны" instead of
  "Не указан"; "alias" is now "other spelling" ("другое написание") in the
  people and organizations screen; the bottom bar says "Люди" so the label is not
  cut off on a phone.
- Stale web application shell: bump the service worker cache to purge older
  cached app shells across deploys, update cached navigation entries on
  successful fetch, and watch `web/dist` in the build script so release builds
  always re-embed updated web assets.
- The archive list shows a document's summary under its title when nothing is searched for (the start of the text only if there is no summary). The search index is rebuilt on the first start after the update.
- A scanned PDF whose only text is the stamp a phone scanner app prints
  ("Scanned with …") is read by OCR and transcribed again: a page whose text
  layer has only a few words is read by OCR too, and once OCR reads clearly
  more, the stamp no longer stands for the page.
- A dropdown inside an opened section of the upload page is drawn over the
  controls below it. A record picked under "People and organizations" fades
  in instead of rising. A lone file on the upload page is asked about
  processing once, by the switch alone.
- The machine-readable zone of the Russian internal passport is read: series
  and number (never a personal number), issue date and names in Cyrillic, with
  every check digit verified. A zone without an expiry date no longer fails.
- When local OCR garbles a zone, a zone that verifies in the model's
  transcription sets the type and values instead; a surname ending in K keeps
  it when the names line runs long.
- The document number read into another identifier (the personal number) is
  dropped instead of failing its checksum.
- Summary sentences that state a year or number the document does not print
  are dropped, so the summary no longer contradicts the fields.
- A known organization the model picks must be backed by the document (an
  office number the document never prints is not a match): by its name or an
  alias in the text, or by the name the model read when it is a close form of
  the stored one; otherwise the printed name is used. A name stored in one
  language and printed only in another needs an alias, or it is added next to
  the stored one until the two are merged.
- With no organization found, a field marked as an issuer field links the
  issuer when the model says it names an organization and not just a place,
  one-word company names included. The flag is on the issuing-authority-like
  fields of the built-in types (existing databases are updated on start), is
  editable on any type in the document type editor and through the type tools.
- A summary amount is checked against the document whatever its thousands and
  decimal separators ("1,250.00" in the document backs "1250" in the summary).
- Facts get a stable snake_case key and a label in the uploader's language,
  shown in the UI instead of the raw key.
- A title no longer stacks two type labels when the MRZ settled a type other
  than the one the model chose.
- Switching archive filters on the first visit of a session no longer blinks
  through an old result (or "nothing found"): a search copy kept on the
  device from an earlier visit is refreshed before the list is drawn, with a
  short wait so a slow or offline device still opens at once.
- Quick filters pick up document types added from another device or by an
  agent instead of waiting for the cached taxonomy to expire.
- Raw MRZ lines that the model reports as numbered facts (`mrz_line_1`…) are
  hidden once their values are extracted, like a single `mrz_line`.
- Memory is handed back to the system as soon as a document is processed:
  the server no longer stays at a hundred-plus megabytes for hours after a
  batch of uploads. Freed memory is returned at once, and transparent huge
  pages are turned off for the process (and the Tesseract runs it starts).
- Faster processing with a lower peak: photos are read at A4 300 DPI size at
  most, two pages of a PDF are read at once, orientation is detected on a
  smaller copy, and each page is held in fewer full-size copies.
- Page images go to the model as the stored WebP instead of a PNG several
  times its size, and the transcription batches of a long document are sent
  in parallel.
- Page renders encode faster (WebP method 4), and page images are checked
  for changes without being read and hashed on every request.
- New sign-in page: the form stands on the page with ruled-line fields, a
  show/hide toggle for the password and a progress sweep on a slow sign-in;
  beside it a small animation of a page being fed in, read and stamped. On a
  phone the form rises over a short header with the button in thumb reach.
- New consent page for connecting an application over OAuth: it names the
  application, lists what it will and will never be able to do, sets apart
  the address it returns to, and matches the app's type and colours in both
  themes. Its error page follows the same look. Still no scripts on either.

### Fixed
- Signing in from an application's consent request now returns to that
  request instead of opening the archive and dropping it.
- Confirmation dialogs keep keyboard focus inside while open (Tab and
  Shift+Tab cycle between their buttons), and the third Directories tab is no
  longer clipped on a 360 px phone.
- A failed upload no longer offers "Try again" when the same file would fail
  again (unsupported type, empty file, too large); plain-text uploads are no
  longer labelled as a PDF text layer.
- The archive's search syntax reference (in the Filters sheet) is clickable:
  every example is a labelled button that closes the sheet and drops the
  snippet into the search field, where it can be combined with the current
  query, edited and run. The old raw one-line
  examples looked like dead input fields and explained nothing.
- A card or passport lying on an A4 scan is cut out of the sheet: the cached
  page and the thumbnail show the document alone, upright, instead of a small
  sideways picture in a corner. The turn is chosen by how each quarter turn
  reads (Tesseract's orientation detection is unreliable on a card, and its
  automatic mode reads sideways text without telling), and specks of dust no
  longer hold the cut-out open.
- The machine-readable zone of such a scan is read: the second pass starts
  one line higher (the first line of an ID card zone has hardly any filler),
  and a zone whose names line was read in Cyrillic look-alikes still fills the
  number, dates, sex and personal number from its check-digit-verified lines.
  Before, the model's misread digits stayed in the fields.
- Opening a document no longer zooms in and back out: the card's thumbnail
  flies to where the page really lands.
- A printed label joined to its value by a comma or a slash ("Autoritatea
  emitentă, București") is cut from field values and correspondent names.
- Titles of foreign-language documents may carry the printed name of the type
  in parentheses ("Удостоверение личности (Carte de identitate)").
- The account language follows the interface language, and a new user starts
  with it. The avatar icon is gone from the sidebar.
- MCP: `delete_tag` reports the tag and how many documents it was removed
  from, search hits say whether the document is in the trash (`trashed`), and
  `get_document` says that `text` is null unless `text_page` is passed.

### Changed
- The interface reads more easily: text follows one scale (no labels below
  12 px, badges at 11 px), every text color meets WCAG AA on every surface it
  sits on in both themes, small uppercase labels became bolder sentence-case
  ones, buttons and inputs share one height and size, and many decorative
  divider lines gave way to spacing.
- Sharing a document asks first and says the link opens for anyone who has it
  for 10 minutes. Moving to trash no longer asks; the toast offers Undo.
- Attention flags explain what happened and what to do instead of showing a
  code. Dates in archive cards follow the interface language and never shift a
  day with the time zone. "Повторить" is now "Переобработать".
- The upload button shows uploads in progress instead of "Upload 0 documents",
  the file picker accepts TXT and Markdown, "restored from trash" stays visible
  after an upload finishes, and a person's name field is labelled "Name".
- Cached page renders and thumbnails are lossy WebP instead of lossless: a
  thumbnail is a few kilobytes (it was 55–159 KB) and a page render stays near
  150 KB (it was up to 729 KB), so the list no longer waits seconds per picture
  over a slow link. Thumbnails are also cropped to the part of the sheet that
  holds content, at most 320 px on the long edge. The image endpoints now send
  an `ETag` and `Cache-Control: private, no-cache`: the browser revalidates for
  a header-only 304, and a reprocess shows up at once.
- Titles and summaries are written in the UI language of the user who uploaded
  the document (English when nobody did), not always in English. Choosing the
  interface language in Settings now also saves it to the account
  (`PATCH /auth/session`), so a user who switched the interface to Russian gets
  Russian titles and summaries; before, only the device remembered the choice. Titles follow
  one shape, `<Type> — <Person>, <year or number>`, with the type label of that
  language and never the correspondent in front; the archive enforces the
  type label and strips a correspondent prefix after the model answers.
- The model's values are normalized after extraction: the sex is `M`, `F` or
  `X`, a day-first date such as `15.04.2012` becomes ISO, a place printed in
  capitals is written as a name, and a printed field label (`Place of birth:`)
  is cut from values and correspondent names.
- The web UI is rebuilt around a screen stack. On a phone the screen you came
  from stays under the one on top: opening a document slides it in (the card's
  thumbnail flies into the page), back slides it out, and a swipe from the left
  edge pulls it back. Tabs cross-fade, the top bar follows the scroll, dialogs
  and the document actions are bottom sheets. The app starts from a cache kept
  on the device (IndexedDB, cleared on sign-out) instead of a loading screen,
  and routes with TanStack Router. The bottom bar has four tabs that fit; the
  trash moved under Settings on a phone.
- Typed fields of a document save when focus leaves them or on Enter, like the
  title and the other metadata; the save button next to each field is gone.
  On a wide screen the page viewer stays in view while you scroll the fields.
  Facts are labelled readably (`Tax ID INN`, not `tax id inn`), with the
  type's own label when there is one.
- The organization page has a single way to add a record: the form opens in
  the detail panel with a Cancel button, instead of a form above the list and
  a second, empty panel beside it.

### Fixed
- Connecting from Claude.ai failed with "couldn't register": its client asks for
  `client_secret_post`, which registration rejected. Registration now accepts
  the secret-based methods and answers `none`, as every registered client is
  public.
- Toasts and notification controls no longer show Russian text in the English
  interface: their texts moved into the locale files.
- A toast no longer covers the actions of the archive selection bar (it rides
  above the bar), and on a phone the bar's close button sits in its top row, so
  the bar is shorter.
- A passport lying sideways on an A4 scan is read from its machine-readable
  zone: when orientation detection finds nothing to go by, or is wrong, and a
  sparse page yields no MRZ at all, the other quarter turns are tried and the
  one whose check digits pass is kept (and cached upright). A names line whose
  `<` filler was miscounted by OCR is now accepted next to a valid second line.
- A correspondent the model could not read or had to infer (for example an
  illegible issuer) is no longer set, a placeholder or bare field label is no
  longer created as a correspondent, and a reprocess that is unsure clears the
  issuer the model set earlier. The prompt no longer matches an authority of
  another country by name similarity.
- Search finds an identifier in any spelling: `AP 1234567`, `AP1234567`,
  `4521 123456` and `4521123456` match the document whether it holds the number
  split or whole (prefixes still do not match).
- Search by type name works in both interface languages: `паспорт`, `passport`
  and `загранпаспорт` find the right documents, and a renamed type label is
  searchable at once.
- Search hits list the key fields in the type's order and show the ones that
  matter (a passport's number and expiry date) instead of an alphabetical
  slice of the first few.
- Snippets and highlights no longer show the Markdown of the transcription.
- A person created after a document was uploaded now finds their documents.
  Creating or renaming a person, and adding, removing or merging aliases, queue
  a background job that matches the people against every stored document by
  name, with no reprocessing and no LLM. Documents whose people were set by
  hand are never touched, and that now includes a list a user cleared on
  purpose, which reprocessing used to refill (new column
  `documents.people_provenance`).
- Resident memory no longer stays at its post-upload peak: the server now
  uses mimalloc, which returns freed OCR and image buffers to the OS instead
  of holding them in per-thread glibc arenas.
- Upright pages with little text, such as an ID card scanned onto a mostly
  blank sheet, are no longer turned over: an orientation verdict below
  Tesseract confidence 2 is ignored.
- A stray vertical scrollbar under the tabs of the organization page.

### Added
- A flagged value says which field it is: `GET /documents/{id}` lists the
  flags each field raises (`fields[].issues`), the card marks that field and
  names it under "Needs attention", and "Value is correct" keeps the value as
  confirmed by hand, which clears the flag. Flags that only reprocessing
  clears offer a Reprocess button.
- A page can be turned in steps of 90° while you read it; the stored scan is
  left as it is.
- A fact can be moved into a field of the document's type from the card.
- Selection in the archive: pick documents (or all that are loaded) and
  reprocess them, add a person or a tag, or move them to trash with Undo.
- "Show documents" on a person, organization or tag opens the archive
  filtered to it.
- Uploads can assign a type, people and tags to every file in the queue
  before it is sent, and show the upload percentage.
- Settings lists the connected OAuth apps (such as Claude) with when they were
  connected and last used, and disconnects them: `GET /connections` and
  `DELETE /connections/{id}`.
- `paperjam rerender [ids…]` rebuilds the cached images of existing documents
  in the new format (no OCR, safe while the server runs, nothing to do the
  second time). Run it once after updating.
- A separate `travel_passport` document type («Загранпаспорт»); a document with a
  valid TD3 machine-readable zone is filed under it, and the internal passport
  stays `passport`. Existing documents move on reprocessing.
- Summary fields per document type (`summary_fields` in the type list), seeded
  for the built-in types and filled in on existing databases at start.
- `POST /api/v1/people/{id}/relink` and the `relink_person` action of
  `manage_taxonomy` repeat that matching on request; a person's documents are
  found with search filtered by `people` (`person:<id>`).
- Passkey (WebAuthn) sign-in for the web UI next to the password: a "Sign in
  with a passkey" button and autofill on the login page, and a Passkeys block in
  settings to add, list and remove your own. It ends in the same session cookie
  as the password login; API tokens and OAuth clients can neither manage nor use
  passkeys. Needs `PAPERJAM_PUBLIC_URL`; built on `webauthn-rs`.
- Deletion endpoints for people and correspondents in the REST API (`DELETE /api/v1/people/{id}` and `DELETE /api/v1/correspondents/{id}`) and delete controls in the web interface.
- Toast notifications and confirmation dialogs in the web UI, replacing native browser alerts.
- Segmented controls and custom select dropdowns for theme, language, and search sorting.
- Cyrillic typography using Golos Text and Geologica, replacing Clear Sans.
- Document intake for PDF, JPEG, PNG, WebP, TIFF, HEIC and plain text, with
  content-addressed storage of unmodified originals, duplicate detection and a
  trash that must be emptied by a person.
- Local recognition: embedded PDF text with a quality score, Tesseract OCR
  (English, Russian, Romanian) with orientation and skew correction, and
  machine-readable-zone parsing with check-digit validation.
- Identifier validators for Russian INN and SNILS and Romanian CNP. The Moldovan
  IDNP is recognised but not checksum-verified (see `docs/validators.md`).
- Optional LLM layer through OpenRouter: transcription of scans, classification,
  typed fields, usage and cost tracking, a monthly budget, and soft failure that
  keeps local results.
- Attention flags raised by automatic checks instead of a review inbox.
- Full-text and field search with Russian, Romanian and English stemming and a
  query language shared by the web page, REST and MCP.
- REST API with OpenAPI, users with argon2id passwords, and API tokens.
- MCP server at `/mcp` (thirteen tools, contract and parity tests), an OAuth 2.1
  authorization server with dynamic client registration, and a stdio proxy.
- CLI: `serve`, `user`, `token`, `backup`, `restore`, `export`, `reindex`,
  `reprocess`, `migrate status`, `mcp-stdio`, `healthcheck`.
- Container image (Debian slim with Tesseract and HEIC support), a Compose
  example and documentation: README, configuration, MCP and architecture guides.
- Web application in English and Russian, with light and dark themes: archive
  search with filters, multi-file uploads, a document view with the original
  and editable fields, management of people, correspondents, types and tags
  with merging, the trash, and settings for users, tokens and LLM spend. It installs as a PWA and keeps its shell offline.
- Workspace skeleton, CI and license.

### Known limitations
- Search does not tolerate OCR typos yet.
- The footprint figures in the README come from a nearly empty archive.
