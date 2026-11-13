# paperjam

**Your passports, contracts and policies in one self-hosted archive, with an
MCP server built in so your AI agent can find, read and file them.**

[![Release](https://img.shields.io/github/v/release/ilodezis/paperjam)](https://github.com/ilodezis/paperjam/releases)
[![CI](https://github.com/ilodezis/paperjam/actions/workflows/ci.yml/badge.svg)](https://github.com/ilodezis/paperjam/actions/workflows/ci.yml)
[![License: AGPL-3.0-or-later](https://img.shields.io/badge/license-AGPL--3.0--or--later-blue.svg)](LICENSE)

**Website: [paperjam.ilodezis.com](https://paperjam.ilodezis.com)**

<!-- DEMO placeholder: a ~30 s recording on synthetic documents goes here (an agent searches, reads a passport field, files a new scan). -->

Ask *"when does my residence permit expire?"* and get a date, not a pile of
PDFs. paperjam reads every upload on your own machine (PDF text, Tesseract OCR,
the machine-readable zone of passports and ID cards), optionally asks an LLM to
classify it and fill typed fields, checks what can be checked, and makes it
searchable.

- **Built for agents.** The MCP server covers the whole document API: search,
  read the text, the fields or the page image, upload, edit, reprocess, trash.
  The one thing it can't do is delete for good.
- **Fields you can trust.** 21 document types, each with its own fields.
  Machine-readable zones are parsed with every check digit verified, and IBANs
  and national ID numbers from ten countries are checksum-validated. A failed
  check flags the document instead of storing a wrong number quietly.
- **One small binary.** Rust, SQLite and an embedded search index. No Redis, no
  Postgres, no extra containers.

It is made for a household or a few people who share one archive. It is not an
office document system: no per-user permissions, no workflow rules.

## Install

Run this on the machine that will keep your documents: a home server, a NAS,
a VPS or your own computer. You need Docker with the Compose plugin.

The installer asks a few questions (install folder; whether you will open
paperjam on that machine, from your network or at your own domain; document
languages; an optional LLM key), starts paperjam and prints the address to open
and the setup code:

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/ilodezis/paperjam/main/install.sh)"
```

Or by hand:

```bash
mkdir paperjam && cd paperjam
curl -fsSL https://raw.githubusercontent.com/ilodezis/paperjam/main/compose.example.yml -o compose.yml
docker compose up -d
docker compose logs paperjam   # prints the setup code
```

### Open it

By hand, paperjam listens on port 8140 of that machine's loopback interface
only, so nothing is exposed until you choose to. Settings go into a `.env` file
next to `compose.yml`; apply them with `docker compose up -d`.

- **On the same computer:** open `http://localhost:8140`. Use Chrome or
  Firefox; Safari refuses the session cookie over plain HTTP on `localhost`.
- **A server, just to try it:** forward the port over SSH from your own
  computer, `ssh -L 8140:localhost:8140 you@server`, and open
  `http://localhost:8140` there.
- **A server on your home network:** set `PAPERJAM_LISTEN=0.0.0.0` and
  `PAPERJAM_PUBLIC_URL=http://<server address>:8140`. This is plain HTTP, so
  keep it off the internet; passkeys need HTTPS and stay off.
- **A VPS, or anything reachable from the internet:** point a domain at it,
  open ports 80 and 443, and set `COMPOSE_PROFILES=https`,
  `PAPERJAM_DOMAIN=docs.example.org` and
  `PAPERJAM_PUBLIC_URL=https://docs.example.org`. A bundled
  [Caddy](https://caddyserver.com) gets the certificate. This is also what
  Claude.ai needs to reach your archive.
- **Behind a reverse proxy you already run:** proxy to `127.0.0.1:8140` and set
  `PAPERJAM_PUBLIC_URL` to its `https://` address
  ([proxy notes](docs/configuration.md#behind-a-reverse-proxy)).

Enter the setup code from the log and pick a username and password. That is
your administrator account; there are no default credentials. Prefer the
command line? `docker compose exec paperjam paperjam user add --admin alice`.

Two more settings most installs want, in the same `.env`:

- `PAPERJAM_OCR_LANGS=eng+deu`: the languages of your documents. The image
  ships English, German, French, Spanish, Italian, Portuguese, Polish, Greek,
  Arabic, Russian, Ukrainian and Romanian.
- `PAPERJAM_LLM_API_KEY=...`: turns on the LLM layer. Read [Privacy](#privacy)
  first.

<details>
<summary><b>Build from source</b></summary>

Clone the repository, copy `compose.example.yml` to `compose.yml`, swap its
`image:` line for the two commented lines below it (`build: .` and
`image: paperjam:local`) and run `docker compose up -d --build`.

Without Docker: `cd web && npm ci && npm run build`, then
`cargo build --release` (add `--features heic` for HEIC photos; needs libheif).
Install Tesseract with the `osd` data and your language packs, set
`PAPERJAM_DATA_DIR` to a writable directory and run `paperjam serve`.

</details>

## Connect an agent

paperjam speaks MCP over HTTP at `/mcp`. Use your `PAPERJAM_PUBLIC_URL` in
place of `https://paperjam.example.org`.

**Claude.ai:** add a custom connector with `https://paperjam.example.org/mcp`.
You sign in and approve it on your own paperjam (OAuth). Claude.ai connects
from Anthropic's servers, so the address must be reachable from the internet.

**Claude Code** and other clients that take a URL and a token:

```bash
docker compose exec paperjam paperjam token create --user alice claude-code
claude mcp add --transport http paperjam https://paperjam.example.org/mcp \
  --header "Authorization: Bearer pj_…"
```

Clients that only speak stdio can use `paperjam mcp-stdio` as a proxy
([how](docs/mcp.md#clients-that-only-speak-stdio)).

Then ask: *"Which passports in the archive expire in the next year?"*, *"What
is the policy number on the car insurance?"*, *"Is last month's bank statement
already in there?"* In Claude.ai, the agent can also show an upload button in
the chat; the files go straight to your server and never through the model.

A token carries its user's full access, so treat it like a password. Document
text is handed to the model as untrusted data, which lowers the risk of
instructions hidden in a document but cannot remove it. Tools, search syntax
and safety details: [docs/mcp.md](docs/mcp.md).

## What it does

- **Takes in** PDF, JPEG, PNG, WebP, TIFF, HEIC and plain text from the web
  page, the REST API, a [watched folder](docs/configuration.md#inbox-folder-optional),
  an [iOS Shortcut](docs/ios-shortcut.md) or an agent. Duplicates are caught by
  content hash.
- **Reads locally, always:** PDF text, Tesseract OCR with deskewing, and ICAO
  9303 machine-readable zones (TD1, TD2, TD3) with check digits.
- **Enriches with an LLM, if you want:** type, title, date, correspondent,
  people, tags, summary and the typed fields of the type. The default is Claude
  Haiku 5.5 via [OpenRouter](https://openrouter.ai); any OpenAI-compatible
  endpoint works. A monthly spend cap is available, and a failed LLM step
  keeps the local results.
- **Flags instead of asking you to review everything:** a document is marked
  only when a check fails, such as a bad checksum or a zone that disagrees with
  the model. Values you edit by hand are never overwritten.
- **Searches** with stemming in 18 languages, phrases, negation and filters on
  any field: `type:passport has:expiring` ([syntax](docs/mcp.md#search-syntax)).
- **Reminds you** through a private calendar feed with each expiry date and a
  renewal reminder before it ([details](docs/configuration.md#calendar-feed)).
- **Stays portable:** one-command backup, a zip export in an open layout, and an
  importer for Paperless-ngx.

The web app is in English and Russian, light and dark, and installs as a PWA.
Sign-in is by password or passkey. A REST API documents itself at
`/api/v1/openapi.json`. How a document flows through the system:
[architecture](docs/architecture.md).

## Privacy

- **Without an LLM key, nothing leaves your machine.** No telemetry, no update
  checks.
- **With a key,** documents go to the LLM provider (OpenRouter by default):
  page images of scans, the text and a small first-page image of digital PDFs,
  and the names of the people, correspondents and tags already in your archive
  so the model can match them. That includes passports and ID cards.
- **Per document, you can opt out.** *Never send to the cloud* keeps a document
  local; the MRZ reader still fills the key fields of identity documents.

An agent you connect reads what you can read, and what it reads goes to its own
provider. paperjam does not encrypt data at rest, so use disk encryption if the
machine can be stolen. Local models and other endpoints:
[configuration](docs/configuration.md#other-openai-compatible-endpoints).

## How it compares

Checked on 2026-11-13 against each project's repository and latest release.

| | paperjam | [Paperless-ngx](https://github.com/paperless-ngx/paperless-ngx) 3.4 | [Papra](https://github.com/papra-hq/papra) | [Readur](https://github.com/readur/readur) |
|---|---|---|---|---|
| Native MCP server | Yes | No; community bridges over REST | No; open feature request | No |
| Structured data | 21 typed document types, MRZ, checksums | Custom fields | Custom properties | Labels |
| Runs on | One binary, SQLite | Django, a database, a Redis-compatible broker | TypeScript, SQLite | Rust, PostgreSQL |
| Email intake | No | Yes | Yes | No |
| License | AGPL-3.0 | GPL-3.0 | AGPL-3.0 | MIT |

Paperless-ngx is the mature choice, with workflows, permissions, email intake,
document versions and, since version 3, built-in LLM features that can run on
a model you host. For an office-style archive, use it. paperjam is for the case
where an agent is a main way in, and the numbers on your documents matter more
than their full text.

## Backups and updates

```bash
docker compose exec -T paperjam paperjam backup /backups/$(date +%F)   # consistent, safe while running
docker compose pull && docker compose up -d                               # update; migrations run on start
```

Mount `/backups` first (the line is commented out in `compose.yml`), and copy
snapshots off the disk. Restore, export and Paperless import:
[configuration](docs/configuration.md#backup-and-restore).

## Documentation

[Configuration](docs/configuration.md) ·
[MCP server](docs/mcp.md) ·
[Architecture](docs/architecture.md) ·
[Validators](docs/validators.md) ·
[iOS Shortcut](docs/ios-shortcut.md) ·
[Releasing and verifying images](docs/releasing.md) ·
[Changelog](CHANGELOG.md)

## Contributing

Bug reports and fixes are welcome; open an issue before anything bigger. Pull
requests come under the CLA in [CONTRIBUTING.md](CONTRIBUTING.md), which also
has the development setup. Report vulnerabilities privately as described in
[SECURITY.md](SECURITY.md).

Licensed under the [GNU AGPL-3.0-or-later](LICENSE).
