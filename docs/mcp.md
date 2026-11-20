# MCP server

paperjam serves MCP over streamable HTTP at `/mcp`. This page is for whoever connects an agent to it, and for people writing a client. It covers the sign-in options, how to connect the common clients, what each of the fourteen tools does, and the search language an agent (or you) can use. The server is stateless: there are no sessions, every request stands alone, and answers are plain JSON, not an event stream.

Every request needs `Authorization: Bearer <token>`. A request without one gets a `401` whose `WWW-Authenticate` header points at the OAuth metadata, which is how remote clients find out how to sign in. A client must also send `Accept: application/json, text/event-stream`, as the protocol requires; without it the server answers `406`.

Two kinds of bearer token work, and both carry the full access of the user they belong to. There are no scopes, so treat a token like a password.

- **API tokens** (`pj_…`) are made in Settings → Access → API tokens, or with `paperjam token create`. The value is shown once and stored as a hash. They can be revoked at any time. The REST endpoint `POST /api/v1/tokens` needs a signed-in web session, so a token cannot create more tokens.
- **OAuth access tokens** (`pjo_…`) go to MCP clients that signed in through the consent page. They expire after an hour and are renewed with a refresh token that lives 90 days and is rotated on every use. They work **only on `/mcp`**, not on the REST API.

To check that a server and token work, list the tools:

```bash
curl -s https://paperjam.example.org/mcp \
  -H "Authorization: Bearer pj_…" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

## Connecting a client

Replace `https://paperjam.example.org` with the value of `PAPERJAM_PUBLIC_URL` in every example.

### Claude.ai (OAuth)

Add a custom connector with the URL `https://paperjam.example.org/mcp`. Claude registers itself and sends you to a consent page on your paperjam, where you sign in and approve it. Then it lists the tools. A client that nobody approved stays inert and is deleted after 24 hours. A client can revoke its own tokens, and Settings → Access → Connected apps lists every connected client so you can disconnect it.

Claude.ai calls your server from Anthropic's side, so the URL must be reachable from the internet. A paperjam that is open only on a LAN or VPN can still be used from Claude Code, which connects from your own machine.

If a connector form insists on a client id and secret, configure a fixed client with the `PAPERJAM_OAUTH_STATIC_*` variables ([configuration](configuration.md#oauth-client-for-mcp-connectors-optional)).

### Claude Code (API token)

```bash
docker compose exec paperjam paperjam token create --user alice claude-code
claude mcp add --transport http paperjam https://paperjam.example.org/mcp \
  --header "Authorization: Bearer pj_…"
```

The first command prints the token once. Without Docker, run `paperjam token create` directly; it needs the same environment as the server. The `claude mcp add` line registers paperjam for the current project, and adding `--scope user` makes it available in every project.

### Clients that only speak stdio

`paperjam mcp-stdio` is a thin proxy with no database access of its own. It forwards MCP messages between stdin/stdout and a running server, so it needs the `paperjam` binary on the machine that runs the client.

```bash
PAPERJAM_TOKEN=pj_… paperjam mcp-stdio --url https://paperjam.example.org
```

As a client configuration:

```json
{
  "mcpServers": {
    "paperjam": {
      "command": "paperjam",
      "args": ["mcp-stdio", "--url", "https://paperjam.example.org"],
      "env": { "PAPERJAM_TOKEN": "pj_…" }
    }
  }
}
```

### If it does not connect

- **`401` with `invalid_token`.** The token is mistyped or was revoked, or it is an OAuth token whose refresh failed. Make a new API token, or disconnect and reconnect the app.
- **`406 Not Acceptable`.** The client does not send both media types in `Accept`.
- **An error page on paperjam instead of the consent page.** For an app that nobody has approved yet, a bad authorization request (a wrong `response_type`, a missing PKCE challenge) shows a page on paperjam instead of redirecting the browser to the address the app registered. Registration itself accepts only HTTPS, localhost and the deep links of known desktop clients as redirect addresses.
- **Links in tool results point at `http://` or the wrong host.** `PAPERJAM_PUBLIC_URL` is not set.

## Tools

The contract (names, descriptions, input and output schemas) is [`src/mcp/tools.json`](../src/mcp/tools.json), and tests call every tool and validate the responses against it. A result is a JSON text block plus `structuredContent`. Lists are wrapped as `{"result": …}` together with `next_cursor`; `search_documents` also returns `total` and `partial`.

| Tool | Purpose |
|---|---|
| `search_documents` | Takes a query string and/or structured `filters` and returns compact hits (title, type, date, due date, key fields, snippet), never full text. Pages of 1 to 50 hits (20 by default) are walked with `cursor`, and `sort` is `relevance`, `date_desc` or `date_asc`. Each hit has `trashed`, true for documents in the trash, which appear only with `include_trashed`. `partial` is true when no document matched every word and the hits match some of them. |
| `get_document` | Metadata, organizations, people, tags, typed fields with provenance, facts, attention flags and file info for one document. `text` is null unless `text_page` (1-based) is given, which returns the text of that page. |
| `get_page_image` | One page as an image with its longest edge at 1600 px, so the model can read the scan itself and double-check digits. |
| `get_download_link` | A URL to the original file that works for 10 minutes, named after the document's title. |
| `share_document` | A link to a page that shows the document without signing in: its pages, a download of the clean copy of a scan (else the original) and, unless `show_details` is false, the summary and extracted values. `ttl` is `15m` (default), `1h` or `24h`. Live links are listed and revoked in the web app. |
| `list_taxonomy` | Document types with their fields, people, correspondents (with aliases) and tags, all with counts. |
| `upload_document` | Adds a file from base64, up to 10 MB decoded. It is the last resort: the file passes through the chat and the model, so it costs tokens. Use it only when the client can neither show the [upload widget](#upload-widget) nor run `curl`. |
| `create_upload_link` | A one-time URL, valid for 15 minutes, that takes one file of any size with `PUT`. In clients that show MCP apps it opens the [upload widget](#upload-widget). Otherwise it returns `url`, `expires_at` and a `curl_example` for an agent with a shell. |
| `update_document` | Patches metadata, people, tags, fields and `local_only`. Only the keys you send change, and `null` clears a value. `people` and `tags` replace the whole list. Everything set here gets provenance `manual` and survives reprocessing. A token can turn `local_only` on but not off; that needs a web session. |
| `reprocess_document` | Runs the local and/or LLM steps again (`local` and `llm` are both true by default), optionally with another `model` for this run, which must be on the server's allowed list. A run that is already queued is not duplicated. |
| `manage_taxonomy` | Creates, renames and merges the vocabulary. `action` picks the operation: `create_person`, `update_person`, `add_alias`, `remove_alias`, `create_correspondent`, `update_correspondent`, `create_tag`, `rename_tag`, `delete_tag`, `create_type`, `update_type`, `merge_people`, `merge_correspondents` or `relink_person` (match a person against the existing documents again). The description lists the arguments each action needs. `delete_tag` returns `item: {deleted, id, name, documents_detached}`. |
| `trash_document`, `restore_document` | Reversible delete, and the way back. |
| `get_stats` | Counts by status, how many documents need attention, the queue (`queued`, `running`, `dead`) and this month's LLM spend. |

Everything the REST API can do except purging has a tool, and the other way round. A test keeps the two in step.

Tool annotations are set honestly (`readOnlyHint`, `destructiveHint`, `idempotentHint`), so a client can let read-only tools run without asking.

A search through the endpoint, as an agent would send it:

```bash
curl -s https://paperjam.example.org/mcp \
  -H "Authorization: Bearer pj_…" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_documents","arguments":{"query":"type:travel_passport has:expiring","limit":5}}}'
```

### Upload widget

In a client that renders MCP apps (Claude.ai does), `create_upload_link` shows a file picker in the chat. Choose one or more scans and the browser sends each file straight to its one-time link with `PUT`, so the bytes never pass through the chat or the model and cost no tokens. The widget asks the host for a fresh link for each extra file. A "keep away from the LLM" checkbox sends `local_only` with the uploads, and when they finish the widget posts a short "uploaded" message back to the chat.

Clients without MCP-app support get the plain result and can upload with `curl`, which is what the `curl_example` field holds:

```bash
curl --fail -T scan.pdf "https://paperjam.example.org/api/v1/ul/<token>?filename=scan.pdf&local_only=true"
```

The query parameters are optional. `filename` names the document, and `local_only=true` keeps it away from the LLM. A link takes one file. A second `PUT` to the same link is refused with `upload_link_invalid`, and so is one after the 15 minutes. A file over `PAPERJAM_MAX_UPLOAD_BYTES` gets a plain-text `413`.

The upload route `/api/v1/ul/{token}` answers CORS requests from any origin, because the widget runs on the client's origin and sends to the server's. The token in the path is the only credential, and the route sets no cookies. The widget declares the server's own origin in its content security policy and nothing else.

## Search syntax

One query language serves the web app, the REST API and `search_documents`. A query is free text, with these additions:

- `"exact phrase"` in double quotes;
- `-word` or `-filter:value` to exclude;
- `OR` between alternatives, and parentheses for grouping. `-` binds tightest, then the implicit AND between terms, then `OR`.

Words are matched against the title, the text of every layer, the summary, the correspondent, people, tags and field values. A document is also found by the name of its type in either interface language (`паспорт`, `passport`, `загранпаспорт`).

### Filters

A filter is `key:value`. Keys are case-insensitive, and a word with an unknown key is plain text. A value with spaces goes in quotes: `from:"Acme Bank"`.

| Filter | Matches |
|---|---|
| `type:` | A document type key, such as `travel_passport`, `id_card`, `contract`, `invoice` or `bank_statement`. `list_taxonomy` returns the keys. `passport` is the internal (domestic) passport, while a passport with a machine-readable zone is `travel_passport`. |
| `person:` | A person's name, where every word must appear in the name or an alias, or a person id. |
| `from:` | A correspondent's name or alias. |
| `tag:` | A tag, compared without regard to case. |
| `local:` | `yes` or `no`: documents marked *Never send to the cloud*, or not. |
| `date:`, `added:`, `due:` | The document date, the day it was added, and its due date, with a comparison (below). |
| `field.<key>` | A typed field of any type, with a comparison: `field.expiry_date<2027-01-01`, `field.total>5000`. |
| `has:` | Whether a property exists: `attention`, `date`, `due`, `tags`, `people`, `summary`, `type`, `from`, or the key of a typed field such as `expiry_date`. Negate it with a minus: `-has:tags`. |
| `has:expiring` | Documents due within 270 days or already past due (see below). |

Comparisons are `>`, `>=`, `<`, `<=` and `=`, written right after the key, with or without a colon: `date>=2024-01` and `date:>=2024-01` are the same. Dates may be `YYYY-MM-DD`, `YYYY-MM` or a bare year, and a period stands for all of its days. So `date:2037` is any day of 2037, `date:<2037` is before 1 January 2037, `>=2037` is from that day on, `<=2037` is up to the end of the year and `>2037` is after it. On a number or money field a bare `2037` is the number 2037. The text filters (`type:`, `person:`, `from:`, `tag:`, `local:`) take no comparison and answer `query_filter` if given one.

Some examples:

```text
type:travel_passport has:expiring
person:testov type:contract -tag:archived
from:"Acme Bank" date:>=2025-01 "account number"
due:<2027-06 OR field.expiry_date<2027-01-01
(invoice OR receipt) -has:attention added:>=2026-10
```

A malformed query is refused with `422` and the code `query_syntax` and the character `position` of the problem in `details`. A well-formed query with an unusable filter value gets `query_filter`.

### Due dates

A document's due date is the day it has to be renewed, replaced or paid. Each built-in type names the field that holds it: identity documents and certificates are due on their `expiry_date` (a travel passport, an ID card, a residence permit, a driver license, another certificate), a rental agreement, a contract and an insurance policy on their `end_date`, a warranty on its `warranty_end_date`, an invoice on its `due_date`. A type of your own is due on whichever of those four fields it defines and the document fills, first in that order. A Russian internal passport prints none. It is replaced on reaching 20 and 45 (Government Decree No. 828 of 1997), so its due date is the holder's 20th birthday when it was issued before that, the 45th birthday when it was issued between the two, and none when it was issued at 45 or later. This needs the passport's birth date and issue date, and either the nationality (Russia) or an interior ministry issuer (МВД or ФМС). Hits and `get_document` carry `due_date`.

`has:expiring` is relative to the day the query runs. Nothing about "soon" is stored, so the same query answers differently next month. `due:` takes the same dates and comparisons as `date:`, for example `due:2027`, `due:<2026-12` or `due:>=2026-01-01`. `has:due` finds every document with a due date, and `-has:due` the ones without.

### How text is matched

When no document contains every word of a query of two or more words, the search runs again with the words joined by `OR`. The response then carries `"partial": true`, and the web app says so above the list. The flag is `false` otherwise, including for a query that was already an `OR` and for a single word. The first pass never drops words: a partial result appears only when the full one is empty.

Text is stemmed for English and the languages the server is configured for, so with Russian and Romanian set up `договор` finds `договора` and `contract` finds `contractul`. A document's type is searched by its labels and, for the built-in types, by the everyday names in Russian, Romanian, German, French, Spanish, Italian and Polish: `Führerschein`, `permis de conduire` and `patente` all find driver's licenses, whatever language the labels are in. Typo tolerance for OCR errors is not implemented.

Identifiers are found in any spelling. `AP 1234567`, `AP1234567` and `ap-1234567` are the same number, and so are `4521 123456` and `4521123456`, whether the document holds them split or whole. Only whole identifiers match, not prefixes. Snippets are plain text; the Markdown of a transcription is stripped.

## Safety properties

- **Agents can never purge.** Purging a document or emptying the trash needs the web app with a cookie session. Tokens, OAuth tokens included, are refused. Trash is reversible.
- **Document content is untrusted.** Text, summaries, file names and page images come from third parties. The server's `instructions` tell the model to treat them as data and never to follow instructions found inside documents. That lowers the risk of prompt injection but cannot remove it, so connect agents you would trust with the same documents.
- **The *Never send to the cloud* switch does not apply to an agent.** It governs paperjam's own LLM calls. An agent connected over MCP can read any document its user can, and what it reads goes to that agent's provider.
