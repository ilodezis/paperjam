# Releasing

A release is a version tag. Pushing it starts the `Image` workflow, which builds the container image for two platforms, signs it, attaches provenance and a software bill of materials, and measures its footprint. This page is for the maintainer cutting a release, and for anyone who wants to verify that an image they pulled came from this repository's workflow.

## Cutting a release

1. Make sure CI is green on the commit you are about to tag. The tag build does not run the tests again.
2. Set `version` in `Cargo.toml` to the new version and refresh `Cargo.lock`. The image is named after the tag, while `paperjam --version` reports the version in `Cargo.toml`, so the two should agree.
3. In `CHANGELOG.md`, rename the `Unreleased` section to the version and date, and add a fresh, empty `Unreleased` section above it.
4. Commit, then tag and push:

   ```bash
   git tag vX.Y.Z
   git push origin main vX.Y.Z
   ```

A tag must start with `v`. A prerelease tag such as `vX.Y.Z-rc.1` also builds, but it does not move `latest`.

## What the workflow does

Four jobs run in order. Pull requests that touch the code or the Dockerfile run the first two, for `amd64` only, and push nothing.

1. **fixtures** generates the synthetic test corpus once and shares it with the later jobs.
2. **build** builds `linux/amd64` and `linux/arm64` on native runners and pushes each by digest, without tags. Before the job ends, it runs Tesseract inside the finished image: it prints the version, checks that the `eng`, `rus`, `ron` and `osd` language packs are present, and reads a Russian fixture page. An image that cannot do that fails the build here, so a broken image never gets a tag.
3. **merge** joins the two digests into one multi-arch manifest and gives it its tags: `X.Y.Z`, `X.Y`, `latest` (stable versions only) and `sha-<short>`. A push to `main` gets only `edge` and `sha-<short>`. This job also signs the image and attaches the attestations described below.
4. **footprint** starts the published `amd64` image, creates a user and a token with the CLI, uploads the synthetic fixtures through the API, waits for the queue to drain and for the process to settle, then writes image size, memory (cgroup and process, idle and peak) and thread count to the run summary. It only reports. It never fails the release.

Tags always trigger the workflow. Pushes to `main` trigger it only when something that ends up in the image changed: the Dockerfile, `Cargo.toml` or `Cargo.lock`, `build.rs`, `src/`, `web/`, `xtask/` or the workflow itself.

## First publish

Two things are needed once, and both are easy to forget:

- A package created by a workflow starts **private**. After the first publish, open the package's settings on GitHub and make it public. Until then, nobody else can pull the image, and the install commands in the README fail for them.
- The `arm64` build runs on `ubuntu-24.04-arm`, which is free for public repositories only. The repository must be public for that job to run.

The image carries the source repository as a label, which links the package to the repository on GitHub.

The README is written for a published image: its install commands pull `ghcr.io/ilodezis/paperjam:latest`. So the order on the day the repository opens is fixed: make the repository public, push the first version tag, wait for the `Image` workflow to finish, make the package public, pull the image on a clean machine and follow the README once. Announce only after that.

## If a job fails

Re-run the failed jobs from the Actions tab; the layer cache makes a second attempt fast. A failure in **build** leaves no tags behind, because only **merge** applies them. Do not move a tag after an image has been published from it. Fix the problem and release the next version instead.

## Verifying an image

Every published image is signed with cosign, keylessly, and the signature is bound to the digest of the multi-arch index. The index also carries build provenance as an attestation, and each platform image carries an SPDX software bill of materials as one. To check an image you pulled:

```bash
IMAGE=ghcr.io/ilodezis/paperjam
DIGEST=$(docker buildx imagetools inspect $IMAGE:latest --format '{{json .Manifest}}' | jq -r .digest)

cosign verify "$IMAGE@$DIGEST" \
  --certificate-identity-regexp '^https://github\.com/ilodezis/paperjam/\.github/workflows/image\.yml@refs/(heads/main|tags/v.*)$' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com

gh attestation verify "oci://$IMAGE@$DIGEST" --repo ilodezis/paperjam
```

The first command proves that the image was signed by `image.yml` in this repository, on `main` or on a version tag. The second proves that GitHub's build provenance for that digest names this repository.

The bill of materials is attached to each platform image, not to the index, so look up the digest of the platform you run:

```bash
AMD64=$(docker buildx imagetools inspect $IMAGE:latest --raw \
  | jq -r '.manifests[] | select(.platform.os == "linux" and .platform.architecture == "amd64") | .digest')

gh attestation verify "oci://$IMAGE@$AMD64" --repo ilodezis/paperjam \
  --predicate-type https://spdx.dev/Document/v2.3
```

To pin a deployment to exactly what you verified, put the digest in the compose file: `image: ghcr.io/ilodezis/paperjam@sha256:…`.
