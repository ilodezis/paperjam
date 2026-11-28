# syntax=docker/dockerfile:1@sha256:572cebbc2b15e6f84c077742952674889cb63cd5e9b80abdffcbe587b6f19183

# Stage 1: the web page, embedded into the binary at build time.
FROM node:lts-trixie-slim@sha256:5adad64f9a1fca2d51972caa65983634c8a774eb300d72b20f7f2a0f60368072 AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm ci
COPY web/ ./
RUN npm run build

# Stage 2: the server, built with HEIC support (needs the system libheif).
FROM rust:1.99-trixie@sha256:8a4e90c7946b74626488b1446f712c0fe0ed1a596cbd3d871684bc9c5433ae07 AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends libheif-dev libclang-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY xtask/ xtask/
COPY src/ src/
COPY --from=web /web/dist web/dist
# The registry and target directory live in BuildKit cache mounts, so a code
# change recompiles the crate itself instead of the whole dependency graph.
# Packed relative relocations (DT_RELR): ~99.7% of the binary's dynamic
# relocations are R_X86_64_RELATIVE, which take 8-24 bytes each in .rela.dyn;
# packed they take a few KB. Needs glibc 2.36+ at run time (trixie has 2.41).
ENV RUSTFLAGS="-C link-arg=-Wl,-z,pack-relative-relocs"
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked --features heic \
    && cp target/release/paperjam /usr/local/bin/paperjam

# Stage 3: runtime. Same Debian release as the build stage, so the libheif
# the binary was linked against is the one it finds.
#
# Tesseract: Debian's `tesseract-ocr` package is the 52 KB command-line tool
# plus 15 training tools, and depends on ICU, glib, cairo, pango, X11 and fonts
# (~58 MB) that paperjam never uses. The language packs only recommend it and
# `tessdata/configs/tsv` ships in libtesseract5, so the library and the packs
# are installed and just the one binary is unpacked from the package. It is
# fetched in the same layer as libtesseract5 so the two versions always match.
#
# Recognition languages: OCR_PACKS lists the Tesseract language codes whose
# packs are installed (`osd` is orientation detection and should stay). Which
# of them are used is a run-time choice, PAPERJAM_OCR_LANGS. To add one that is
# not listed, build the image yourself:
#   docker build --build-arg OCR_PACKS="eng osd jpn" -t paperjam .
# (an underscore in a code, as in chi_sim, becomes a hyphen in the Debian
# package name; the script below does that).
FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2
ARG OCR_PACKS="eng osd deu fra spa ita por pol ell ara rus ukr ron"
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        libheif1 libheif-plugin-libde265 \
        libtesseract5 \
        $(for lang in $OCR_PACKS; do echo "tesseract-ocr-$lang" | tr _ -; done) \
    && cd /tmp \
    && apt-get download tesseract-ocr \
    && dpkg-deb --fsys-tarfile tesseract-ocr_*.deb | tar -x -C / ./usr/bin/tesseract \
    && rm -f /tmp/*.deb \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin paperjam \
    && mkdir /data \
    && chown paperjam:paperjam /data

COPY --from=build /usr/local/bin/paperjam /usr/local/bin/paperjam

USER paperjam
WORKDIR /data
VOLUME /data
EXPOSE 8140
ENV PAPERJAM_BIND=0.0.0.0:8140

HEALTHCHECK --interval=30s --timeout=10s --start-period=30s --retries=3 \
    CMD ["paperjam", "healthcheck"]

# `docker compose exec paperjam paperjam user add --admin <name>` and the other
# commands work as-is; without arguments the container runs the server.
ENTRYPOINT ["paperjam"]
CMD ["serve"]
