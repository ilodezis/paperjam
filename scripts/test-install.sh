#!/usr/bin/env bash
# Runs install.sh against a fake `docker` and `curl`, so the questions, the
# files it writes and the final message can be checked without Docker.
#   bash scripts/test-install.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin"

cat >"$work/bin/docker" <<'STUB'
#!/usr/bin/env bash
echo "docker $*" >>"$FAKE_LOG"
case "$*" in
  "compose version" | "info") exit 0 ;;
  "compose ps --format {{.Health}} paperjam") echo healthy ;;
  "compose logs --no-log-prefix paperjam") echo "  Open http://localhost:8140 and enter the setup code: ab12-cd34-ef56" ;;
esac
STUB
cat >"$work/bin/curl" <<'STUB'
#!/usr/bin/env bash
# install.sh calls: curl -fsSL <url> -o compose.yml
out=""
while [ $# -gt 0 ]; do
  if [ "$1" = "-o" ]; then out="$2"; fi
  shift
done
cp "$FAKE_COMPOSE" "$out"
STUB
chmod +x "$work/bin/docker" "$work/bin/curl"

export PATH="$work/bin:$PATH" FAKE_LOG="$work/log" FAKE_COMPOSE="$root/compose.example.yml" HOME="$work/home"
mkdir -p "$HOME"
fail() { echo "FAIL: $*" >&2; exit 1; }

run() { # run NAME [VAR=value ...]
  local name="$1"; shift
  : >"$FAKE_LOG"
  env -u PAPERJAM_INSTALL_DIR -u PAPERJAM_ACCESS -u PAPERJAM_HOST -u PAPERJAM_OCR_LANGS -u PAPERJAM_LLM_API_KEY \
    PAPERJAM_INSTALL_DIR="$work/$name" "$@" bash "$root/install.sh" --yes </dev/null >"$work/$name.out" 2>&1 ||
    { cat "$work/$name.out"; fail "$name: install.sh failed"; }
}

# Defaults: this computer only, nothing but the OCR language in .env.
run local
grep -q '^PAPERJAM_OCR_LANGS=eng$' "$work/local/.env" || fail "local: OCR language"
! grep -q 'LISTEN\|PUBLIC_URL\|COMPOSE_PROFILES' "$work/local/.env" || fail "local: unexpected settings"
grep -q 'Setup code:  ab12-cd34-ef56' "$work/local.out" || fail "local: setup code not shown"
grep -q '^docker compose up -d$' "$FAKE_LOG" || fail "local: not started"
case "$OSTYPE" in msys* | cygwin*) ;; *) [ "$(stat -c %a "$work/local/.env")" = 600 ] || fail "local: .env must be 600" ;; esac

# Home network.
run network PAPERJAM_ACCESS=network PAPERJAM_HOST=192.0.2.10
grep -q '^PAPERJAM_LISTEN=0.0.0.0$' "$work/network/.env" || fail "network: listen"
grep -q '^PAPERJAM_PUBLIC_URL=http://192.0.2.10:8140$' "$work/network/.env" || fail "network: url"
grep -q 'Open:        http://192.0.2.10:8140' "$work/network.out" || fail "network: message"

# A domain turns on the HTTPS proxy and carries the optional settings.
run domain PAPERJAM_ACCESS=domain PAPERJAM_HOST=docs.example.org PAPERJAM_OCR_LANGS=eng+deu PAPERJAM_LLM_API_KEY=sk-test
for line in PAPERJAM_DOMAIN=docs.example.org COMPOSE_PROFILES=https PAPERJAM_PUBLIC_URL=https://docs.example.org PAPERJAM_OCR_LANGS=eng+deu PAPERJAM_LLM_API_KEY=sk-test; do
  grep -qx "$line" "$work/domain/.env" || fail "domain: missing $line"
done

# Refuses to overwrite an install.
if PAPERJAM_INSTALL_DIR="$work/domain" bash "$root/install.sh" --yes </dev/null >"$work/again.out" 2>&1; then
  fail "a second install over the first must fail"
fi
grep -q 'already holds a paperjam install' "$work/again.out" || fail "again: message"

# Bad answers fail early.
if PAPERJAM_INSTALL_DIR="$work/bad" PAPERJAM_ACCESS=network bash "$root/install.sh" --yes </dev/null >"$work/bad.out" 2>&1; then
  fail "network without an address must fail"
fi
if PAPERJAM_INSTALL_DIR="$work/bad2" PAPERJAM_ACCESS=nonsense bash "$root/install.sh" --yes </dev/null >"$work/bad2.out" 2>&1; then
  fail "an unknown access mode must fail"
fi

echo "install.sh: all checks passed"
