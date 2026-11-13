#!/usr/bin/env bash
# paperjam installer: asks a few questions, writes compose.yml and .env, starts
# paperjam with Docker Compose and prints the code that creates your account.
#
#   bash -c "$(curl -fsSL https://raw.githubusercontent.com/ilodezis/paperjam/main/install.sh)"
#
# It only automates what the README describes; nothing here is needed to run
# paperjam. Every question can be answered up front through an environment
# variable (the name is in brackets), and `--yes` accepts all defaults, which
# is how to run it without a terminal:
#
#   PAPERJAM_INSTALL_DIR  where the files go                 [~/paperjam]
#   PAPERJAM_ACCESS       local, network or domain           [local]
#   PAPERJAM_HOST         the address or domain people use   [none]
#   PAPERJAM_OCR_LANGS    Tesseract languages, e.g. eng+deu  [eng]
#   PAPERJAM_LLM_API_KEY  optional LLM key (see Privacy)     [none]
#   PAPERJAM_REF          git ref to take compose.example.yml from [main]
set -euo pipefail

REPO_RAW="https://raw.githubusercontent.com/ilodezis/paperjam"
PORT=8140

assume_yes=false
for arg in "$@"; do
  case "$arg" in
    -y | --yes) assume_yes=true ;;
    -h | --help)
      sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "unknown option: $arg" >&2
      exit 2
      ;;
  esac
done

say() { printf '%s\n' "$*"; }
die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

# Prompts are read from the terminal even when the script itself is piped in.
interactive=false
if ! $assume_yes && [ -r /dev/tty ] && [ -w /dev/tty ]; then
  interactive=true
fi

# ask VAR "question" "default" [secret]: keeps a value that is already set in
# the environment, otherwise asks (or takes the default when not interactive).
ask() {
  local var="$1" question="$2" default="$3" secret="${4:-}" answer=""
  if [ -n "${!var:-}" ]; then
    return
  fi
  if $interactive; then
    if [ -n "$secret" ]; then
      printf '%s ' "$question" >/dev/tty
      IFS= read -rs answer </dev/tty || true
      printf '\n' >/dev/tty
    else
      printf '%s [%s] ' "$question" "$default" >/dev/tty
      IFS= read -r answer </dev/tty || true
    fi
  fi
  printf -v "$var" '%s' "${answer:-$default}"
}

command -v docker >/dev/null 2>&1 || die "Docker is not installed. See https://docs.docker.com/engine/install/"
docker compose version >/dev/null 2>&1 || die "the Docker Compose plugin is missing (docker compose). See https://docs.docker.com/compose/install/"
docker info >/dev/null 2>&1 || die "Docker is installed but not running, or you may not use it (is your user in the docker group?)"
command -v curl >/dev/null 2>&1 || die "curl is required"

say "paperjam installer"
say ""

ask PAPERJAM_INSTALL_DIR "Install into which folder?" "$HOME/paperjam"
dir="${PAPERJAM_INSTALL_DIR/#\~/$HOME}"
if [ -e "$dir/compose.yml" ] || [ -e "$dir/.env" ]; then
  die "$dir already holds a paperjam install. To update it: cd $dir && docker compose pull && docker compose up -d"
fi

if $interactive; then
  say ""
  say "Where will you open paperjam?"
  say "  local   only on this computer (http://localhost:$PORT)"
  say "  network on your home or office network, over plain HTTP (no passkeys)"
  say "  domain  on the internet at your own domain, with HTTPS from Let's Encrypt"
fi
ask PAPERJAM_ACCESS "Choose one:" "local"
case "$PAPERJAM_ACCESS" in
  local | network | domain) ;;
  *) die "PAPERJAM_ACCESS must be local, network or domain, not '$PAPERJAM_ACCESS'" ;;
esac

listen="127.0.0.1"
public_url=""
domain=""
case "$PAPERJAM_ACCESS" in
  network)
    ask PAPERJAM_HOST "The address of this computer on the network (for example 192.0.2.10):" ""
    [ -n "$PAPERJAM_HOST" ] || die "the network address is required (set PAPERJAM_HOST)"
    listen="0.0.0.0"
    public_url="http://${PAPERJAM_HOST}:$PORT"
    ;;
  domain)
    ask PAPERJAM_HOST "Your domain (it must already point at this machine; ports 80 and 443 must be open):" ""
    [ -n "$PAPERJAM_HOST" ] || die "the domain is required (set PAPERJAM_HOST)"
    domain="$PAPERJAM_HOST"
    public_url="https://${domain}"
    ;;
esac

ask PAPERJAM_OCR_LANGS "Languages of your documents (Tesseract codes joined with +, e.g. eng+deu+fra):" "eng"
if $interactive && [ -z "${PAPERJAM_LLM_API_KEY:-}" ]; then
  say ""
  say "Optional: an OpenRouter API key adds titles, summaries and typed fields."
  say "Page images and text are then sent to the LLM provider (see Privacy in the README)."
  say "Leave it empty to keep everything on this machine."
fi
ask PAPERJAM_LLM_API_KEY "LLM API key (input hidden):" "" secret

ref="${PAPERJAM_REF:-main}"
mkdir -p "$dir"
cd "$dir"
curl -fsSL "$REPO_RAW/$ref/compose.example.yml" -o compose.yml ||
  die "could not download compose.example.yml from $REPO_RAW/$ref"

umask 077
{
  say "# Written by install.sh. Every setting is documented in .env.example of the repository."
  say "PAPERJAM_OCR_LANGS=$PAPERJAM_OCR_LANGS"
  [ "$listen" = "127.0.0.1" ] || say "PAPERJAM_LISTEN=$listen"
  [ -z "$public_url" ] || say "PAPERJAM_PUBLIC_URL=$public_url"
  if [ -n "$domain" ]; then
    say "PAPERJAM_DOMAIN=$domain"
    say "COMPOSE_PROFILES=https"
  fi
  [ -z "${PAPERJAM_LLM_API_KEY:-}" ] || say "PAPERJAM_LLM_API_KEY=$PAPERJAM_LLM_API_KEY"
} >.env

say ""
say "Starting paperjam in $dir ..."
docker compose pull
docker compose up -d

# The container reports healthy once the server answers.
say "Waiting for paperjam to come up ..."
healthy=false
for _ in $(seq 1 90); do
  state="$(docker compose ps --format '{{.Health}}' paperjam 2>/dev/null || true)"
  if [ "$state" = "healthy" ]; then
    healthy=true
    break
  fi
  sleep 2
done
$healthy || die "paperjam did not become healthy. See: cd $dir && docker compose logs paperjam"

url="${public_url:-http://localhost:$PORT}"
code="$(docker compose logs --no-log-prefix paperjam 2>/dev/null | sed -n 's/.*setup code: //p' | tail -n 1)"
say ""
say "paperjam is running."
say "  Open:        $url"
if [ -n "$code" ]; then
  say "  Setup code:  $code"
  say "  Enter the code on that page and choose your username and password."
else
  say "  An account already exists; sign in with it."
fi
say ""
say "Update later:   cd $dir && docker compose pull && docker compose up -d"
