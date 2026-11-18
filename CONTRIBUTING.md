# Contributing to voxix

Thank you for your interest in contributing to voxix, an open source local-first speech-to-text application!

## Ways to contribute

1. **Code** — bug fixes, new features, and platform support.
2. **Translations** — see [CONTRIBUTING_TRANSLATIONS.md](CONTRIBUTING_TRANSLATIONS.md).
3. **Bug reports** — open a GitHub issue with the bug report template.
4. **Documentation** — fix any stale or missing doc you find,.

## Getting started

Set up a build environment with [BUILD.md](BUILD.md). Then:

```bash
bun install
bun run tauri dev
```

Before you open a PR, make sure the following commands pass on your machine:

```bash
bun run format:check
bun run lint
bunx tsc --noEmit
bun run check:translations   # only when changes touch ui/i18n
cargo check --manifest-path core/Cargo.toml --all-targets
cargo test  --manifest-path core/Cargo.toml --all-targets
```

## Programming notes

- **Frontend TypeScript/React**
  - Strict TypeScript; every user-facing string goes through i18next with a key in `ui/i18n/locales/en/translation.json`.
  - Functional components with hooks; styles follow `ui/voxix/theme.css` with the `voxix-` class prefix (no Tailwind).
- **Rust backend**
  - Handle errors explicitly; avoid `unwrap()` outside tests.
  - Format with `cargo fmt`; reusable behavior goes into tools, managers, or commands with comments that explain intent, not mechanics.
- **Generated files are never hand-edited:** `ui/bindings.ts`, `bun.lock`, `core/Cargo.lock` are regenerated from their sources. `core/src/catalog/catalog.json` is the exception: it is hand-maintained and was hand-pruned to the mirrored set in commit 77874fe. Do NOT run scripts/gen_catalog.py to regenerate it — that would restore ~212 artifact entries that are not hosted anywhere. Extend it only by explicit owner instruction.

## Submitting changes

1. Create a branch from the tip of the main branch.
2. Make your change with focused commits. Use conventional commit prefixes (`feat:`, `fix:`, `refactor:`, `docs:`, `chore:`) and write messages that explain _why_.
3. Run the verification commands above.
4. Open a pull request using the repository's PR template — a short summary, how you tested the change, and any notes on generated artifacts or migrations.
5. Keep PRs scoped to one change. Link related issues in the description.

For feature requests, open an issue first so the approach can be discussed before work starts.

## License

By contributing to voxix you agree that your contributions will be licensed under the MIT License. See [LICENSE](LICENSE).

## Questions

Open a GitHub issue via the template (blank issues are disabled) — for a bug use the bug report template, otherwise describe the topic in the template. Owner: [sahilchouksey/voxix](https://github.com/sahilchouksey/voxix).
