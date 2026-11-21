# Email intake

Forward or send an email to an address on your domain and have its attachments land in paperjam.

paperjam does not run an SMTP server of its own or poll IMAP mailboxes. Instead, it exposes a single webhook endpoint — `POST /api/v1/ingest/email` — that accepts a raw RFC 5322 email (`message/rfc822`), verifies a write-only secret and an optional sender allowlist, and hands each extracted file to the normal upload pipeline (size check, content-hash deduplication, local OCR/MRZ and optional LLM enrichment).

The usual bridge in front of it is a small **Cloudflare Email Worker** (~45 lines, free plan), which receives mail on an address such as `docs@example.org`, checks DMARC and the sender allowlist at the edge, and streams the raw message to paperjam. Any other mail server or script (Postfix, Exim, Stalwart, `curl`) can `POST` to the same endpoint directly.

## 1. Enable the endpoint in paperjam

Set `PAPERJAM_EMAIL_SECRET` in `.env` (at least 32 random bytes, e.g. `openssl rand -base64 48`), plus any filters you want, and restart the server:

```sh
PAPERJAM_EMAIL_SECRET=replace-with-at-least-32-random-bytes
PAPERJAM_EMAIL_ALLOWED_SENDERS=alice@example.org,@partner.example
PAPERJAM_EMAIL_TAG=email
```

| Variable | Default | Meaning |
|---|---|---|
| `PAPERJAM_EMAIL_SECRET` | unset (feature off) | Shared write-only bearer secret (`Authorization: Bearer …`). Must be at least 32 bytes. Can only submit mail; cannot read, list or modify documents. |
| `PAPERJAM_EMAIL_USER` | the only active user | Username that owns ingested documents. Optional while only one active user exists; required when there are several. |
| `PAPERJAM_EMAIL_ALLOWED_SENDERS` | unset (any sender) | Comma-separated list of exact addresses (`alice@example.org`) and `@domain` suffixes (`@example.org`). Checked against `X-Envelope-From` when supplied, otherwise against all `From` addresses. |
| `PAPERJAM_EMAIL_BODY_FALLBACK` | `true` | When an email has no eligible attachments (for instance, a receipt or booking confirmation in the body), convert the headers and body text to a `.md` document. Set `false` to ignore attachment-less emails. |
| `PAPERJAM_EMAIL_LOCAL_ONLY` | `false` | Set `true` to mark every emailed document *Process on this server only* (no LLM calls). |
| `PAPERJAM_EMAIL_TAG` | unset | Tag name (up to 64 characters) created if missing and attached to every ingested document. |

Setting any secondary `PAPERJAM_EMAIL_*` variable without `PAPERJAM_EMAIL_SECRET` stops the server at startup so a half-configured setup never goes unnoticed.

Verify the endpoint locally with any `.eml` file before wiring up DNS:

```bash
curl -s -X POST https://paperjam.example.org/api/v1/ingest/email \
  -H "Authorization: Bearer $PAPERJAM_EMAIL_SECRET" \
  -H "Content-Type: message/rfc822" \
  --data-binary @message.eml
```

## 2. What gets ingested (and what is skipped)

paperjam reads the MIME tree recursively (including forwarded `message/rfc822` messages up to 5 levels deep, up to 50 attachments per email):

- **Kept:** PDFs and images (JPEG, PNG, WebP, TIFF, HEIC) with `Content-Disposition: attachment` or without a disposition (many scanners omit it on PDFs); explicit `.txt` and `.md` attachments; and inline photos outside `multipart/related` without a `Content-ID` of at least 4 KB — which is how iPhone Mail attaches photos.
- **Skipped:** images inside `multipart/related` or carrying a `Content-ID` (HTML email logos and signatures) unless explicitly marked `Content-Disposition: attachment`, inline images smaller than 4 KB (tracking pixels and icons), empty files, and service or unsupported formats (`.ics` calendar invites, `.vcf` vCards, `.p7s`/`.p7m`/`.asc`/`.sig` signatures, `winmail.dat`, Office files, CSVs).
- **No eligible attachments:** if `PAPERJAM_EMAIL_BODY_FALLBACK=true` (the default) and the email has a readable text or HTML body, paperjam formats the subject, sender, UTC date and stripped body text as a `.md` file and ingests it. If the email had attachments that were all duplicates of documents already in the archive, body fallback does **not** run.

## 3. Set up a Cloudflare Email Worker

Using Cloudflare Email Routing keeps passwords and OAuth apps out of paperjam and lets you use a short address like `docs@example.org` without a random suffix, because the Worker checks Cloudflare's own DMARC verification before forwarding anything.

### Prerequisites

- A domain on Cloudflare with **Email Routing** enabled. (Email Routing adds MX records to the root domain. If another provider already handles mail at your apex domain, [configure a subdomain](https://developers.cloudflare.com/email-service/configuration/subdomains/) such as `inbox.example.org` instead.)
- Your paperjam instance reachable over HTTPS (`https://paperjam.example.org`).
- Inbound message size within Cloudflare Email Routing's **25 MiB** limit.

### Why DMARC + allowlist replaces a secret address

Email `From:` headers can be forged by any sender. Before invoking an Email Worker, Cloudflare validates SPF, DKIM and DMARC and prepends its own `Authentication-Results` header starting with `mx.cloudflare.net;`. Requiring `dmarc=pass` in that first header alongside an allowlist of your own addresses ensures forged mail and random spam are rejected at Cloudflare's edge before reaching your server or spending LLM budget.

### Create and deploy the Worker

1. In the Cloudflare dashboard, open **Compute (Workers)** → **Create Worker** (or deploy with Wrangler) and paste the script below:

```js
export default {
  async email(message, env) {
    // Only trust the first Authentication-Results header prepended by Cloudflare.
    const rawAuth = (message.headers.get("authentication-results") || "").trimStart();
    const firstHop = rawAuth.split(/,\s*(?=[a-z0-9.-]+;)/i)[0];
    if (!firstHop.startsWith("mx.cloudflare.net;") || !/;\s*dmarc=pass\b/i.test(firstHop)) {
      message.setReject("DMARC verification required");
      return;
    }

    const allowed = (env.ALLOWED_SENDERS || "")
      .split(",")
      .map((s) => s.trim().toLowerCase())
      .filter(Boolean);
    const sender = message.from.trim().toLowerCase();
    const ok =
      allowed.length === 0 ||
      allowed.some((rule) =>
        rule.startsWith("@")
          ? sender.endsWith(rule) && sender.length > rule.length
          : sender === rule,
      );
    if (!ok) {
      message.setReject("Sender is not allowed");
      return;
    }

    const response = await fetch(`${env.PAPERJAM_URL.replace(/\/+$/, "")}/api/v1/ingest/email`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${env.EMAIL_SECRET}`,
        "Content-Type": "message/rfc822",
        "X-Envelope-From": message.from,
      },
      body: message.raw,
    });

    if (response.status >= 500) {
      // Throw so the sender sees a temporary failure instead of a permanent bounce.
      throw new Error(`paperjam upstream error: ${response.status}`);
    }
    if (!response.ok) {
      message.setReject(`Rejected by archive (${response.status})`);
    }
  },
};
```

2. In the Worker's **Settings → Variables and Secrets**, add:
   - `PAPERJAM_URL`: `https://paperjam.example.org`
   - `EMAIL_SECRET` (**Secret**): the same value as `PAPERJAM_EMAIL_SECRET`
   - `ALLOWED_SENDERS` (optional): comma-separated list of your addresses, e.g. `alice@example.org`
3. Under **Email → Email Routing → Routing rules**, create a custom address (for example `docs@example.org`) and set its action to **Send to a Worker**, choosing the Worker you just created.

> **Important:** Cloudflare passes the full raw email through the Worker before reaching your server. If you do not want a third-party mail relay in the path, feed the endpoint from your own mail server instead (next section).

## 4. Without Cloudflare (Postfix, Stalwart, scripts)

Any mail server that can pipe a raw message to a command, or any script that fetches mail, can call the same endpoint directly:

```bash
curl -fsS -X POST https://paperjam.example.org/api/v1/ingest/email \
  -H "Authorization: Bearer $PAPERJAM_EMAIL_SECRET" \
  -H "Content-Type: message/rfc822" \
  -H "X-Envelope-From: $SENDER" \
  --data-binary @-
```

When `X-Envelope-From` is omitted, paperjam validates `PAPERJAM_EMAIL_ALLOWED_SENDERS` against every address in the email's `From` header instead.

## Response format

`POST /api/v1/ingest/email` returns `200 OK` with a JSON summary:

```json
{
  "documents": [
    {
      "id": "0190d3a0-0000-7000-8000-000000000000",
      "filename": "bill.pdf",
      "status": "queued",
      "restored": false
    }
  ],
  "duplicates": [
    {
      "document_id": "0190d3a0-0000-7000-8000-000000000001",
      "filename": "already-archived.pdf"
    }
  ],
  "skipped": [
    { "filename": "logo.png", "reason": "inline_asset" },
    { "filename": "invite.ics", "reason": "unsupported_type" }
  ],
  "used_body_fallback": false
}
```

## Security

- **`PAPERJAM_EMAIL_SECRET` is write-only.** Unlike a `pj_…` API token, it cannot search, read, edit, or delete documents. Never put a user API token into a Cloudflare Worker or mail script.
- **Brute-force protection.** Failed secret attempts are rate-limited per client address with exponential backoff (`429 too_many_attempts`). Behind a reverse proxy, make sure `PAPERJAM_TRUSTED_PROXIES` and `PAPERJAM_CLIENT_IP_HEADER` are configured ([proxy notes](configuration.md#behind-a-reverse-proxy)).
- **Sender allowlist + budget.** Use `PAPERJAM_EMAIL_ALLOWED_SENDERS` (and `dmarc=pass` in the Worker) together with `PAPERJAM_LLM_MONTHLY_BUDGET_USD` if your address is easy to guess. Set `PAPERJAM_EMAIL_LOCAL_ONLY=true` if emailed documents should never go to an LLM.
- **Unauthenticated bodies are not buffered.** The secret and rate limiter are checked before reading the request body, and oversized payloads fail at `PAPERJAM_MAX_UPLOAD_BYTES`.

## Troubleshooting

- **`401 invalid_email_secret`.** `EMAIL_SECRET` in the Worker does not match `PAPERJAM_EMAIL_SECRET` in `.env`, or the `Authorization: Bearer …` header is missing.
- **`403 sender_not_allowed`.** Check `PAPERJAM_EMAIL_ALLOWED_SENDERS` against the `X-Envelope-From` address (for auto-forwarded mail, some providers rewrite the envelope sender to a VERP bounce address; pass the original `From` or use an explicit forwarding rule in your mailbox rather than server-level silent redirects if needed).
- **`413 file_too_large`.** The message exceeds `PAPERJAM_MAX_UPLOAD_BYTES` (or Cloudflare's 25 MiB limit, or a reverse proxy's `client_max_body_size` — nginx defaults to 1 MB).
- **`422 invalid_email`.** The body is empty or not a valid RFC 5322 email (for example, raw PDF bytes sent instead of a `.eml` message).
- **`503 email_not_configured` / `email_user_unconfigured`.** Either `PAPERJAM_EMAIL_SECRET` is unset, or there are multiple active users and `PAPERJAM_EMAIL_USER` is unset or disabled.
- **Cloudflare Worker fails with `Too many redirects`.** In Cloudflare → **SSL/TLS → Overview**, set encryption mode to **Full (strict)**.
- **Forwarded HTML newsletter without PDF created a `.md` document.** Set `PAPERJAM_EMAIL_BODY_FALLBACK=false` if you only want real file attachments.
