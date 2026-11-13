# Security policy

paperjam stores identity documents, contracts and medical papers, so security
reports come before everything else.

## Supported versions

Until 1.0, only the latest release and `main` get security fixes.

## Reporting a vulnerability

**Do not open a public issue, pull request or discussion for a vulnerability.**

Use GitHub's private vulnerability reporting: open the repository's **Security**
tab and choose **Report a vulnerability**
(<https://github.com/ilodezis/paperjam/security/advisories/new>). Only you and
the maintainers can see the report. Please include:

- what is affected (version or commit, how you run it) and what an attacker
  gains: data read, data changed, access taken over, service stopped;
- steps to reproduce, ideally a proof of concept or a failing request, built on
  synthetic documents and never on real personal ones.

## What to expect

- Acknowledgement within 3 working days.
- A status update (confirmed, need more detail, or not a vulnerability) within
  14 days, then updates as the fix progresses.
- Coordinated disclosure: we agree a date with you, 90 days after the report at
  the latest. The fix ships as a release first, then the advisory is published.
- Credit in the advisory and the changelog if you want it.

## Scope

In scope:

- sign-in, sessions, passkeys, OAuth, API tokens, the MCP endpoint and the REST
  API, including anything that lets a token or an agent do what only a signed-in
  web session may;
- upload links, download links, public share links and the calendar feed link;
- parsing of untrusted PDFs and images, which runs in a separate child process
  (`paperjam extract-worker`): escaping it, or a crafted file that takes the
  server down;
- documents or their text reaching a signed-out visitor, through search,
  thumbnails, exports, links or logs;
- the published container image and its release pipeline.

Out of scope:

- an instance exposed without TLS, with a weak or leaked `PAPERJAM_SECRET_KEY`,
  or otherwise deployed against `docs/configuration.md`;
- missing security headers on a reverse proxy you operate;
- one signed-in user seeing another's documents: the archive is shared by
  design among a small household that trusts each other;
- bugs that exist only in an outdated release or a fork;
- denial of service by an authenticated administrator, and volumetric flooding;
- findings that need an already compromised host or physical access;
- scanner output without a demonstrated impact.

## Security model

paperjam is a single binary. Everything lives in one data directory on a
volume: a SQLite database, the uploaded originals and a rebuildable tantivy
search index. It is built to sit behind a reverse proxy that terminates TLS.

- Users have separate logins. Passwords are hashed with Argon2id; API tokens
  are random, shown once and stored only as SHA-256 hashes. The web page also
  accepts passkeys (WebAuthn). MCP clients use an API token or OAuth
  (authorization code with PKCE); a client that registers itself stays inert
  until a signed-in user approves it.
- A token cannot permanently delete documents, mint tokens or create users;
  those need a web session or the server CLI.
- LLM enrichment is off until you set an API key. When on, the text of every
  document not marked *local only* (and the page images of scans) goes to the
  configured API root, OpenRouter by default, identity documents included. What
  leaves the machine is the operator's decision.
- PDFs and images are parsed in a child process with an empty environment, a
  time limit, a memory cap (on Linux) and no core dumps, so a malformed file
  ends that process rather than the server.
- The image runs as an unprivileged user. The compose example binds the port to
  loopback, with a read-only root file system and all capabilities dropped.
- Images on the GitHub Container Registry are signed keylessly with cosign and
  carry build provenance and SBOM attestations; `docs/releasing.md` shows how to
  verify them.

## Safe harbour

If you act in good faith, test only against your own instance, stay away from
other people's data and keep disruption to a minimum, we will not pursue or
support legal action against you for your research, and we will work with you
to fix what you find.
