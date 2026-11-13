# Contributing to paperjam

Thanks for wanting to help. Bug reports, fixes and small improvements are all
welcome. For anything bigger than a fix, open an issue first so we can agree on
the shape before you spend time on it.

## Licensing and the CLA

paperjam is licensed under the GNU AGPL-3.0-or-later (see `LICENSE`). The
maintainer may also offer paperjam as a paid hosted service, and may ship
separate closed-source modules next to the open code. To keep that possible,
every outside contribution comes under this Contributor License Agreement:

1. You keep the copyright in your contribution.
2. You grant the maintainer (GitHub: `ilodezis`) a perpetual, worldwide,
   non-exclusive, royalty-free, irrevocable license to use, modify, sublicense
   and distribute your contribution under any license, including proprietary
   ones.
3. Your contribution stays available to everyone under the AGPL-3.0-or-later
   as part of the project.
4. You confirm that you wrote the contribution, or have the right to submit it,
   and that it contains no personal documents, secrets or code copied from a
   source with an incompatible license.

You accept the CLA by writing this sentence in the pull request description:

> I have read CONTRIBUTING.md and agree to the Contributor License Agreement.

A pull request without it cannot be merged.

## Before you send a pull request

- Everything in the repository is in English: code, comments, commit messages.
- Test fixtures are synthetic. Never commit a real document, name or number
  (`cargo xtask fixtures` generates the corpus).
- Use conventional commits (`feat:`, `fix:`, `docs:` ...).
- Run the full gate locally; it must be green:

  ```bash
  cargo fmt --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo xtask fixtures
  cargo test --workspace
  cargo deny check            # when dependencies change
  cd web && npm ci && npm test && npm run build
  ```

- Add a test with every behaviour change, and a line to `CHANGELOG.md` for
  anything a user would notice.

## Security issues

Do not open a public issue for a vulnerability. Report it privately through
GitHub's "Report a vulnerability" button on the repository's Security tab.
