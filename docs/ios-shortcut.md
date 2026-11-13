# iOS Shortcut

A home-screen web app cannot receive files from the iOS share sheet, which is what you want when you have just photographed a letter or received a PDF in a mail app. An Apple Shortcut can. It appears in the share sheet and sends each item to the REST API with a personal token. This page is for anyone who wants that on an iPhone or iPad, and it also describes the upload request in enough detail to reproduce it in another tool. Android is covered at the end.

The shortcut is not shipped as a file, because `.shortcut` files must be signed by Apple. You build it by hand once, which takes a few minutes.

Replace `https://paperjam.example.org` with your public URL and `pj_…` with your token wherever they appear.

## The upload request

The whole API is described at `/api/v1/openapi.json`. The shortcut needs one call:

- **Request:** `POST /api/v1/documents`, as `multipart/form-data`.
- **Authentication:** the header `Authorization: Bearer pj_…`. A personal API token works here, as it does on `/mcp`.
- **The file** goes in a part named `file`. One file per request, so several files mean several requests.
- **Options** are optional text parts: `local_only` (`true` or `false`) keeps the file away from the LLM; `tags[]` and `people[]` are ids of existing tags and people (repeat the part for several); `type_id` is the id of a document type. The ids come from `GET /api/v1/taxonomy` or the `list_taxonomy` tool, and a value that is not an id is refused with `422` and `invalid_form`.
- **Order matters.** Send the options before the `file` part. The handler returns as soon as it has read `file`, so an option that comes after it is silently ignored. The request still succeeds, which makes this easy to miss.
- **Success:** `201` with `{"id": "…", "status": "queued", "restored": false}`. `restored` is `true` when the same bytes were in the trash and came back.
- **Duplicate:** `409` with `code: "duplicate"`. The id of the document that already exists is in `details.document_id` and also at the top level as `document_id`.
- **Too large:** a file over `PAPERJAM_MAX_UPLOAD_BYTES` (100 MiB by default) gets `422` with `code: "file_too_large"`.
- **Other errors:** `401` with `invalid_token` for a missing, mistyped or revoked token, and `422` with `invalid_form` for an unknown form field.

Before building anything on the phone, try the request from a computer. If this works, the shortcut will too:

```bash
curl -s -H "Authorization: Bearer pj_…" \
  -F "local_only=true" \
  -F "file=@scan.pdf" \
  https://paperjam.example.org/api/v1/documents
```

## 1. Create a token for the phone

Use a token of its own, so that you can revoke the phone without touching your laptop. In the web app, open Settings → Access → API tokens, create a token named `iphone-shortcut` and copy it. On the server, the equivalent is:

```bash
docker compose exec paperjam paperjam token create --user alice iphone-shortcut
```

The token (`pj_…`) is shown once. Keep a copy in your password manager as well, since you will paste it into the shortcut.

## 2. Build the shortcut

In the Shortcuts app, create a new shortcut and name it **Send to paperjam**. The names of some options differ between iOS versions; where they do, pick the option that matches the description.

1. Open the shortcut's details (the ⓘ or sliders icon) and turn on **Show in Share Sheet**.
2. Set **Share Sheet Types** (or **Accepted types**) to **Files, Images and PDFs**, and leave the other types off.
3. Optional: set **If There's No Input** so that running the shortcut without a file asks you to pick one.
4. Add **Repeat with Each** and set its input to **Shortcut Input**. Every file then runs through the actions below once.
5. Inside the loop, add **Get Contents of URL** and set it up:
   - **URL:** `https://paperjam.example.org/api/v1/documents`
   - **Method:** `POST`
   - Under **Headers**, add the key `Authorization` with the value `Bearer pj_…`. Keep the space after `Bearer`.
   - **Request Body:** `Form`. Add a field with **Key** `file`, **Type** `File` and **Value** set to the **Repeat Item** variable.
6. Optional: add **Show Notification** after the request, with its body set to the **Contents of URL** variable. On success it shows `{"id": …, "status": "queued"}`, and on failure the error code. **Get Dictionary Value** for `id` gives you only the id.

To try it, share a photo, a PDF or a file from the Files app and choose **Send to paperjam**. The new document appears in the web app straight away, queued at first and ready once it has been read.

### Variant: always local-only

To keep everything this shortcut uploads away from the LLM, add a form field **above** the `file` field:

- **Key** `local_only`, **Type** `Text`, **Value** `true`.

The order of the rows in the form is the order they are sent in, and a field below `file` is ignored.

## Troubleshooting

- **The shortcut is not in the share sheet.** Check the details: **Show in Share Sheet** must be on, and the accepted types must include what you are sharing. A photo is an image, a scan from the Files app is usually a PDF.
- **`401` with `invalid_token`.** The header is missing or has no space after `Bearer`, or the token was revoked. The value must start with `pj_`, and the header belongs under **Headers**, not in the form body.
- **`422` with `file_too_large`.** The file is over the server's limit. Raise `PAPERJAM_MAX_UPLOAD_BYTES` in `.env` and restart, or split the document.
- **A bare `413` with no JSON body.** That does not come from paperjam, which answers with `file_too_large`. A reverse proxy in front of it has a body limit of its own; nginx allows 1 MB unless told otherwise. Raise it in the proxy.
- **`409` with `duplicate`.** The same bytes are already in the archive, and nothing was stored twice. Use `document_id` to open the existing document.
- **HEIC photos.** Send them as they are. The container image reads HEIC, and the original is stored untouched.
- **The server cannot be reached.** The phone must be able to open `https://paperjam.example.org`. Away from home that means either a public URL behind your reverse proxy or a VPN on the phone (WireGuard or Tailscale, for example) that routes to the server.

## Security

The token is stored inside the shortcut, and it is a full API token, not an upload-only one. Whoever has it can search, read, edit and trash documents as you. Do not share the shortcut, and revoke the token if the phone is lost or the shortcut leaks.

Revoke it in Settings → Access → API tokens, where the list shows the token names. It stops working immediately.

If you would rather not keep a full token on the phone, the other iPhone route is the web app itself. Upload from there, pick **Files**, and scan with the Files app (**⋯ → Scan Documents**).

## Android

The same `POST` works from apps that can send an HTTP request with a form body, such as HTTP Shortcuts. Use the same URL, the same `Authorization` header and a `file` form part, and put any `local_only` field before it.
