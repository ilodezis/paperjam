# Configuration

paperjam is configured through environment variables, and none of them is required: a first start with no configuration at all works. This page is for the person who runs the server. Every variable is here with its default, and the last section lists the symptoms that a wrong value causes. Installing is covered in the [README](../README.md#install); [`.env.example`](../.env.example) is the file to copy.

Variables come from the process environment and from a `.env` file in the working directory. A variable that is already set in the environment wins over the file, and an empty value counts as unset. With the compose file from the repository, `env_file: .env` hands the file to the container (the file is optional there). A value that cannot be used stops the server at startup with a message that names the variable, so a typo shows up on the first start.

A minimal `.env` for a server behind a reverse proxy:

```sh
PAPERJAM_PUBLIC_URL=https://paperjam.example.org
```

Add `PAPERJAM_LLM_API_KEY` to switch on the [LLM layer](#llm-layer-optional). Everything else has a default that suits a household archive.

## Server

| Variable | Default | Meaning |
|---|---|---|
| `PAPERJAM_SECRET_KEY` | generated | At least 32 bytes; `openssl rand -base64 48` gives 64 characters. It signs session cookies, OAuth access tokens, download links, share links and the setup code. Unset, the server creates a random key in `<data dir>/secret.key` (mode 0600) on first start and reuses it; `paperjam backup` copies that file and `restore` puts it back. A variable wins over the file. Changing the key signs everybody out and ends all live links. API tokens are stored as hashes and keep working. |
| `PAPERJAM_ADMIN_USER`, `PAPERJAM_ADMIN_PASSWORD` | unset | Create the first administrator at start-up when no user exists yet, for scripts and app stores. Both or neither. Later starts ignore them, so remove them once the account exists (the password is otherwise visible in `docker inspect`). Without them, the first administrator is created on the web page with the setup code that the server prints to its log at every start while no user exists, or with `paperjam user add --admin`. |
| `PAPERJAM_DATA_DIR` | `/data` | Database, originals, derived files and the search index, described in [architecture](architecture.md#data-directory). Use a local disk: SQLite's write-ahead log is not safe on a network file system. Keep the database and `originals/` together; a start with originals but no database deletes them. |
| `PAPERJAM_BIND` | `127.0.0.1:8140` | Listen address. The container image sets `0.0.0.0:8140`. Leave that alone and publish the port on the host's loopback interface instead, as `compose.example.yml` does. |
| `PAPERJAM_PUBLIC_URL` | unset | The address people and MCP clients use to reach the server. It is the base of OAuth metadata, download links, share links and upload links, and the relying party for passkeys (which need `https://`). It also decides the session cookie: an `https://` address gets the `Secure`, host-only `__Host-` cookie; an `http://` address (a home network without TLS) gets a plain one that works over HTTP. Without it, links are built as `http://` plus the `Host` header of the request, which is wrong behind a TLS proxy, and the cookie is `Secure` whenever the server listens beyond loopback. Set it whenever a proxy is involved. |
| `PAPERJAM_MAX_UPLOAD_BYTES` | `104857600` (100 MiB) | Size limit for one file. Larger files are refused with `file_too_large`. |
| `PAPERJAM_TRUSTED_PROXIES` | `127.0.0.0/8,::1/128,172.16.0.0/12` | Comma-separated addresses or CIDR ranges of reverse proxies. See [Behind a reverse proxy](#behind-a-reverse-proxy). |
| `PAPERJAM_CLIENT_IP_HEADER` | `x-forwarded-for` | The header a trusted proxy puts the client address in. |
| `PAPERJAM_CALENDAR_LEAD_DAYS` | `60` | How many days before a due date the [calendar feed](#calendar-feed) adds a "renew" event. `0` leaves only the event on the due date. |
| `PAPERJAM_LOG_FORMAT` | `text` | `json` writes one JSON object per log line on stderr. The level comes from the standard `RUST_LOG` variable (default `info`; for example `RUST_LOG=paperjam=debug`). |

Logs carry ids and error codes. They never contain document text, field values, passwords or tokens.

### Sessions and cookies

The session cookie is marked `Secure` when `PAPERJAM_PUBLIC_URL` is an `https://` address, or when the server listens on anything other than a loopback address. Browsers send a `Secure` cookie over plain HTTP only to `localhost`, so run behind TLS.

Over HTTPS the cookie is called `__Host-pj_session`. The `__Host-` prefix makes the browser keep it for this exact host, so a neighbouring subdomain can neither read it nor plant one. On a plain-HTTP local setup the name is `pj_session`. A request that changes something and carries the cookie is refused unless it comes from paperjam's own pages (checked with `Sec-Fetch-Site`, or with `Origin` in browsers that do not send it).

### Behind a reverse proxy

Sign-in attempts are rate-limited per client address and username. Behind a proxy every connection arrives from the proxy, so paperjam needs the real address from a header, and it believes that header only when the connection itself comes from an address in `PAPERJAM_TRUSTED_PROXIES`.

`X-Forwarded-For` is read from the right, skipping trusted proxies, so a client cannot choose its own address by sending the header itself. The default range list covers a proxy on the same host and a proxy that reaches a container through a Docker network gateway, which is the usual Caddy or nginx setup. If a CDN sits in front of the proxy, the proxy sees the CDN's address, not the visitor's. Set `PAPERJAM_CLIENT_IP_HEADER` to the header the CDN adds (`cf-connecting-ip` behind Cloudflare), and make sure only the CDN can reach the proxy, otherwise anyone can send that header.

### Passkeys

Passkey (WebAuthn) sign-in has no variable of its own. It is on whenever `PAPERJAM_PUBLIC_URL` is an `https://` address with a host name, such as `https://paperjam.example.org`. That host becomes the relying party ID and that origin is the only one a passkey answers for, so open paperjam at exactly that address. A passkey made there does not work through a LAN IP address or another name.

Browsers refuse IP addresses as relying parties and require a secure context, so an IP address or a plain `http://` URL leaves passkeys off. The one exception is `http://localhost`, for development. When passkeys are unavailable the settings page says what is missing, and the password login keeps working.

If you change the host of `PAPERJAM_PUBLIC_URL`, the passkeys made for the old host stop working. Remove them in settings and add new ones. Passkeys are stored as public keys in the database and go into backups with it.

## Inbox folder (optional)

The inbox folder is off unless `PAPERJAM_INBOX_DIR` is set. paperjam then polls that directory and adds every file that appears in it, exactly as if it had been uploaded in the browser: same type check, size limit, duplicate detection and processing.

| Variable | Default | Meaning |
|---|---|---|
| `PAPERJAM_INBOX_DIR` | unset | The directory to watch, subdirectories included (up to 16 levels deep). It must not contain `PAPERJAM_DATA_DIR` or sit inside it; this is checked after resolving `..` and symlinks, and the server refuses to start otherwise. |
| `PAPERJAM_INBOX_USER` | the only user | Username the documents are attributed to. Optional while exactly one active user exists. With several users and no value, the scanner logs one error and stays idle. |
| `PAPERJAM_INBOX_INTERVAL_SECS` | `30` | Seconds between scans, from 1 to 86400. |

Setting `PAPERJAM_INBOX_USER` or `PAPERJAM_INBOX_INTERVAL_SECS` without `PAPERJAM_INBOX_DIR` is an error, so a half-configured inbox does not sit there looking active.

A container needs the folder mounted, and the mount must be writable by uid 10001, because the scanner deletes and moves files:

```yaml
services:
  paperjam:
    environment:
      PAPERJAM_INBOX_DIR: /inbox
    volumes:
      - ./inbox:/inbox
```

How the scanner treats files:

- **Polling.** File system notifications do not fire on network mounts or for many sync clients, so the directory is listed every interval.
- **Half-written files wait.** A file is taken only when its size and modification time are the same on two consecutive scans, and its modification time is at least twice the scan interval in the past (never less than 10 seconds). A slow network client that pauses mid-copy is therefore not ingested half-written. A file that changes while it is being read is kept and ingested again as new content.
- **Ignored.** Hidden files and directories (names starting with `.`, which covers `.syncthing.*` and `.~lock*`), names starting with `~`, `*.part` and `*.tmp` files, symbolic links and empty files.
- **Added files are deleted.** Once the original and its database record are on disk and synced, so a power loss cannot take the only copy, the source file is removed. The archive's copy is the original. A file that is already in the archive is removed too. If the removal fails, for example because of permissions, the problem is logged once and the file is left alone until it changes.
- **Rejected files are moved, not retried.** An unsupported type, an oversize file or content that cannot be used goes to `<inbox>/.failed/` with a `<name>.reason.txt` beside it. A file that cannot be read at all, say because of a network share hiccup, is not rejected. It stays where it is and is tried again later.
- **Errors that are not the file's fault**, such as a full disk, are logged once. The file stays and is retried after it changes or the server restarts.

## Recognition

| Variable | Default | Meaning |
|---|---|---|
| `PAPERJAM_OCR_LANGS` | `eng` | Tesseract languages of your documents joined with `+`, for example `eng+deu`. More languages make recognition slower and can confuse similar scripts, so list only the ones you have. It also picks the search stemmers: English always, plus every listed language that has one (Arabic, Danish, Dutch, Finnish, French, German, Greek, Hungarian, Italian, Norwegian, Portuguese, Romanian, Russian, Spanish, Swedish, Tamil, Turkish). Changing it rebuilds the search index at the next start. The container image ships packs for `eng`, `deu`, `fra`, `spa`, `ita`, `por`, `pol`, `ell`, `ara`, `rus`, `ukr`, `ron` and `osd` (orientation detection). For another language, build the image with `docker build --build-arg OCR_PACKS="eng osd jpn" -t paperjam .`; the argument lists the Tesseract codes whose packs are installed. |
| `PAPERJAM_OCR_WORKERS` | `1` | Documents recognized at the same time. Each one runs several Tesseract processes, so raise it only on a machine with spare cores. |
| `PAPERJAM_EXTRACT_TIMEOUT_SECS` | `120` | How long the child process that parses a PDF or image may stay silent before it is killed. The document then fails with `pdf_corrupt` or `image_corrupt`. Must be at least 1. |
| `PAPERJAM_EXTRACT_MEMORY_MB` | `1536` | Address-space limit of that child process, on Linux only; `0` removes it. It is set before the process starts and counts all of its address space, the allocator's reservations included. A 100-megapixel photo needs about 600 MB. Keep the limit below the container's memory limit (2 GB in `compose.example.yml`). |

Why a document is parsed in a separate process is explained in [architecture](architecture.md#parsing-in-a-child-process).

## Calendar feed

Settings → Access → *Calendar subscription* makes a private link of the form `/calendar/<secret>.ics`. Subscribe to it in a calendar app and every document with a due date (see `has:expiring` in the [search syntax](mcp.md#due-dates)) shows up as an all-day event, plus a "renew" event `PAPERJAM_CALENDAR_LEAD_DAYS` days earlier (60 by default; `0` turns it off).

The feed carries titles and dates only (and a link to the document when `PAPERJAM_PUBLIC_URL` is set), never a field value such as a document number, and leaves out documents marked *Never send to the cloud*. The secret opens this feed and nothing else, is stored hashed and is shown once; rotate it to get a new link, or revoke it. It is part of the URL, so it appears in reverse-proxy access logs. Google Calendar fetches the feed from Google's servers, so it cannot reach a paperjam that is only open over a VPN; Apple Calendar fetches it from the device and can.

## LLM layer (optional)

Without `PAPERJAM_LLM_API_KEY` only local recognition runs, and nothing leaves the machine. With a key, every document that is not marked *Never send to the cloud* goes through the LLM steps, **identity documents included**. Scanned pages are sent to the LLM API as images. Born-digital PDFs skip transcription and are sent as text plus one small image of page 1, so that an issuer printed only as a logo can still be read. The README's [Privacy section](../README.md#privacy) lists exactly what is sent.

The default is [OpenRouter](https://openrouter.ai) with Claude Haiku 5.5. Any other OpenAI-compatible chat-completions endpoint (OpenAI, Gemini's OpenAI-compatible endpoint, Ollama, vLLM, a proxy of your own) works too; see [Other OpenAI-compatible endpoints](#other-openai-compatible-endpoints).

| Variable | Default | Meaning |
|---|---|---|
| `PAPERJAM_LLM_API_KEY` | unset | Turns the LLM layer on. It is never stored in the database or shown in the web app. The older name `PAPERJAM_OPENROUTER_API_KEY` still works; when both are set, this one wins. |
| `PAPERJAM_LLM_BASE_URL` | `https://openrouter.ai/api/v1` | API root of the chat-completions endpoint; paperjam appends `/chat/completions`. The older name `PAPERJAM_OPENROUTER_BASE_URL` still works; when both are set, this one wins. |
| `PAPERJAM_LLM_PROVIDER` | `auto` | The request dialect: `openrouter`, `generic`, or `auto` to pick by the host of the base URL (`openrouter.ai` is OpenRouter, every other host is generic). Set it when a proxy or local gateway in front of OpenRouter should still get the OpenRouter fields. |
| `PAPERJAM_LLM_PRICES` | unset | A price table for endpoints that report no cost, so that the spend budget works. Entries are `model=input:output`, separated by commas, in USD per million tokens, for example `gpt-4.1-mini=0.40:1.60,llama3.1:8b=0:0`. A model id is matched exactly as written in `PAPERJAM_LLM_MODEL`. |
| `PAPERJAM_LLM_MODEL` | `anthropic/claude-haiku-5.5` | Model for classification and field extraction. The default is an OpenRouter id; on another endpoint, use the name that endpoint expects. |
| `PAPERJAM_LLM_VISION_MODEL` | same as `PAPERJAM_LLM_MODEL` | Model for transcribing scanned pages. It must accept images. |
| `PAPERJAM_LLM_ALLOWED_MODELS` | the two configured models | Comma-separated models that a reprocess request may name in its `model` field. Normal processing always uses the configured models, listed or not. |
| `PAPERJAM_LLM_REASONING_EFFORT` | OpenRouter: `none` on transcription and field extraction, `low` on classification (falling back to `low` on HTTP 400); elsewhere: not sent | Reasoning effort passed to the model. Setting a value overrides every step. On other endpoints it goes out as the top-level `reasoning_effort` string, and only when you set it, because many models and servers reject the field. |
| `PAPERJAM_LLM_WORKERS` | `2` | Documents sent to the LLM at the same time. |
| `PAPERJAM_LLM_CREATE_TAGS` | `true` | Whether the model may create new tags: at most two per document, short lower-case topic words in the output language, never one that repeats a document type, a person or an organization. An existing tag with the same name, compared without regard to case, is used instead. `false` limits the model to existing tags. This switch never lets it create people. Accepts `true`, `false`, `1`, `0`, `yes`, `no`, `on`, `off`. |
| `PAPERJAM_LLM_MONTHLY_BUDGET_USD` | unset (no limit) | Spend limit per calendar month (UTC), checked before each LLM step. See [Budget](#budget). |

To use the default setup, one line is enough:

```sh
PAPERJAM_LLM_API_KEY=sk-or-...
```

Documents added before the key was set are not touched. Run `paperjam reprocess --all --llm` to send them through the LLM steps.

### Other OpenAI-compatible endpoints

Point `PAPERJAM_LLM_BASE_URL` at the API root, set `PAPERJAM_LLM_API_KEY` (a local server that ignores the key still needs some value) and name the models:

```sh
PAPERJAM_LLM_BASE_URL=https://api.openai.com/v1
PAPERJAM_LLM_API_KEY=sk-...
PAPERJAM_LLM_MODEL=gpt-4.1-mini
PAPERJAM_LLM_PRICES=gpt-4.1-mini=0.40:1.60
```

For a server on the Docker host, such as Ollama or vLLM, remember that `localhost` inside the container is the container itself. Use `http://host.docker.internal:11434/v1`, and on Linux add `extra_hosts: ["host.docker.internal:host-gateway"]` to the service. Or run the model server as another service in the same compose file and use its service name.

> **Quality is only guaranteed on the default model.** The prompts, schemas and checks are tuned on Claude Haiku 5.5 through OpenRouter. In the project's own comparison on the structured-extraction task, weaker and local models failed: invalid or incomplete JSON, wrong fields, missed documents. Nothing blocks you from trying them, but check the results before you rely on them.

What differs from OpenRouter:

- **The request.** Only standard fields go out: `model`, `messages`, `max_tokens`, `response_format` (`json_schema`, strict) and, if you set one, `reasoning_effort`. The OpenRouter-only fields are not sent: the `provider` routing flags, which pin upstreams that honour the schema and do not retain prompts, plus `reasoning` and `usage`. What the endpoint does with your documents is therefore its own policy. The endpoint must support structured outputs (`response_format` with `json_schema`). Models that need `max_completion_tokens` instead of `max_tokens` are not supported.
- **The cost.** paperjam takes `usage.cost` when the endpoint returns one. Otherwise it prices the call from `PAPERJAM_LLM_PRICES`: input and output tokens times the model's price. With neither, the call is recorded with its token counts and **zero cost**, one warning is logged, and the monthly budget **cannot protect that setup**. It compares recorded cost, so it never runs out. Price every model you use, the vision model included. A local model is `model=0:0`.
- **The wording.** Errors and logs say "The LLM API" instead of "OpenRouter".

### Budget

`PAPERJAM_LLM_MONTHLY_BUDGET_USD` is compared with the cost recorded since the first of the month, UTC. It is checked before each LLM step, so the step that crosses the line still runs and the overshoot is at most one step. After that the remaining steps are skipped, and documents are flagged `llm_budget` and keep their local results. The flag clears when the document is reprocessed with the LLM steps in a month with budget left. The current month's spend is shown by `get_stats` and `GET /api/v1/stats`.

### When the LLM call fails

A request is retried up to three times, with a doubling delay, on `408`, `429`, any `5xx`, a timeout (120 seconds) or a connection error. A `Retry-After` header is honoured up to 30 seconds. If the call still fails, or the answer does not fit the schema, the LLM phase stops there. The document keeps its local results and every step that already finished, and gets the `llm_failed` flag. After you fix the cause (a wrong key, an empty account, a model that does not exist), run `paperjam reprocess --llm <id>` or use the reprocess button.

## OAuth client for MCP connectors (optional)

Most MCP clients register themselves through Dynamic Client Registration, and you need nothing here. Some connector forms ask for a client id and secret instead. For those, configure one fixed client. All three variables or none:

| Variable | Meaning |
|---|---|
| `PAPERJAM_OAUTH_STATIC_CLIENT_ID` | The client id to enter in the connector. |
| `PAPERJAM_OAUTH_STATIC_CLIENT_SECRET` | At least 32 bytes. |
| `PAPERJAM_OAUTH_STATIC_REDIRECT_URIS` | The redirect URIs the client may use, separated by commas, spaces or newlines. Compared exactly. |

Setting only some of them stops the server at startup.

## Command line

Every `paperjam` subcommand reads the same environment as the server and opens the data directory directly. In a container, run them with `docker compose exec paperjam paperjam <command>`, so they see the same volume and variables.

| Command | What it does |
|---|---|
| `serve` | Runs the server and the background workers. This is what the container runs by default. |
| `user add [--admin] [--display-name <name>] [--locale en\|ru] <username>`, `user list`, `user disable <username>`, `user enable <username>`, `user delete <username>`, `user passwd <username>` | Manages users: creates accounts, lists them, disables or re-enables sign-in, permanently deletes an account (keeping archive documents), and resets a password. |
| `token create --user <username> <label>` | Prints a new API token, once. |
| `backup <dir>`, `restore [--force] <dir>` | Writes a consistent snapshot into an empty directory, and restores one into an empty data directory. Details in [Backup and restore](#backup-and-restore). |
| `export <file.zip>` | Writes the originals and open JSON and text metadata as a zip. |
| `import paperless <export-dir> [--dry-run] [--user <username>]` | Imports a Paperless-ngx export; see [Importing from Paperless-ngx](#importing-from-paperless-ngx). |
| `reindex` | Rebuilds the search index from the database. |
| `reprocess [--all] [--llm] [<id>...]` | Queues documents for reprocessing; a running server picks them up. Local steps always run. The LLM steps run only with `--llm`, unlike the web app, REST API and MCP tools, where they are on by default. `--all` skips documents in the trash. |
| `rerender [<id>...]` | Rebuilds cached page renders and thumbnails in the current format. Safe to run while the server is up. |
| `migrate status` | Lists the database migrations and whether they are applied. Migrations themselves run at server start. |
| `mcp-stdio --url <server>` | A proxy between a stdio MCP client and a running server; see [MCP](mcp.md#clients-that-only-speak-stdio). |
| `healthcheck [--url <url>]` | Exits 0 when `GET /healthz` answers 200. The container's health check runs it. |

Two variables are read by single commands only and do not belong in a long-lived `.env`:

| Variable | Used by | Meaning |
|---|---|---|
| `PAPERJAM_NEW_PASSWORD` | `user add`, `user passwd` | The password for the account, for scripts. Without it the command prompts twice. |
| `PAPERJAM_TOKEN` | `mcp-stdio` | The API token to present to the server. It keeps the token out of the process list. |

## Backup and restore

Only the database and the originals are irreplaceable; page renders and the search index are derived and rebuilt. `paperjam backup <empty-dir>` writes a consistent snapshot of the database, the originals and `secret.key`, and is safe while the server runs. `paperjam export <file.zip>` writes the originals with open JSON and text metadata, for leaving paperjam rather than for restoring it.

With the compose file, uncomment the `./backups:/backups` volume line in `compose.yml` (the host directory must be writable by uid 10001), then run, for example from cron:

```bash
docker compose exec -T paperjam paperjam backup /backups/$(date +%F)
```

Snapshots hard-link the originals when the file system allows, so a snapshot on the same disk does not protect against losing that disk: copy it elsewhere, and encrypt copies that leave your control (they contain `secret.key`).

To restore, stop the server, restore into the volume and start it again; the search index is rebuilt on start:

```bash
docker compose stop paperjam
docker compose run --rm --no-deps paperjam restore --force /backups/<date>
docker compose up -d
```

`restore` refuses a data directory that already holds data unless `--force` is given; on a fresh volume leave it out.

## Importing from Paperless-ngx

Export with Paperless's own `document_exporter` (`--split-manifest` and `--use-filename-format` both work; unzip a `--zip` export first), make the export directory visible to paperjam (in a container, mount it), then:

```bash
paperjam import paperless <export-dir> --dry-run   # parse and report, write nothing
paperjam import paperless <export-dir>             # --user <name> when there are several administrators
```

Each original goes through the normal upload path: stored byte for byte, local OCR and MRZ, and the LLM steps when a key is set. Paperless's archived PDF/A copies are not imported. Title, created date, correspondent and tags are kept as values set by hand, so the LLM steps do not overwrite them. paperjam's document types are a fixed set, so a Paperless document type becomes the tag `paperless:<type>`; notes are not imported. Files already in the archive are skipped, one failing document does not stop the run, and running it again is safe. If a file is stored but applying its metadata fails, the summary lists it as "stored, but ..." for you to fix by hand.

## Troubleshooting

**The server exits at startup naming a variable.** The value is missing or cannot be parsed. The message says which, for example `PAPERJAM_SECRET_KEY is invalid: must be at least 32 bytes`.

**A few failed sign-ins throttle every user at once.** paperjam sees only the proxy's address, so all visitors share one failure count. Check that the proxy sends `X-Forwarded-For` (or the header named in `PAPERJAM_CLIENT_IP_HEADER`) and that the proxy's address is inside `PAPERJAM_TRUSTED_PROXIES`.

**Download or share links start with `http://`, or point at the wrong host.** `PAPERJAM_PUBLIC_URL` is unset or wrong.

**The passkey option is missing in settings.** `PAPERJAM_PUBLIC_URL` is not an `https://` address with a host name, or you opened paperjam at a different address than the one it names.

**Documents carry `llm_failed`.** The LLM call failed. Look in the logs for the status code, fix the key, base URL or model name, and reprocess with `--llm`. **`llm_budget`** means the monthly budget ran out.

**A big photo or scan fails with `image_corrupt` or `pdf_corrupt`.** If the file opens fine elsewhere, the parser may have hit `PAPERJAM_EXTRACT_MEMORY_MB` or `PAPERJAM_EXTRACT_TIMEOUT_SECS`. Raise the matching limit, staying below the container's memory limit.

**Files in the inbox folder are not picked up.** Wait two scan intervals, since a file is ignored until it has stopped changing. Then check the name (hidden, `~`, `.part` and `.tmp` files are skipped), the logs for a permissions error, and, with several users, `PAPERJAM_INBOX_USER`.

**An upload fails with a bare `413`.** When a file is over `PAPERJAM_MAX_UPLOAD_BYTES`, `POST /api/v1/documents` answers `422` with `file_too_large` in a JSON body, so a plain `413` there comes from a reverse proxy with a body limit of its own (nginx defaults to 1 MB). Raise the limit in the proxy. The one-time upload link is the exception: paperjam itself answers a plain-text `413` for an oversize file.
