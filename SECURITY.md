# Security Policy

## Reporting a vulnerability

Do not open a public issue. Use GitHub's private vulnerability reporting:
[Open a private advisory](https://github.com/sahilchouksey/voxix/security/advisories/new).

## Supported versions

Only the latest release (currently 0.1.x) receives fixes.

## Verifying release artifacts

Every release attaches a `SHA256SUMS` and a minisign signature
(`*.minisig`) for each artifact. The public key is in
[`packaging/minisign.pub`](packaging/minisign.pub). You can also verify
build provenance with `gh attestation verify <file> --repo sahilchouksey/voxix`.

## Known transitive advisories (accepted, not exploitable in voxix)

These come from upstream toolchains and cannot be patched independently right now:

| Advisory                     | Crate       | Path                                                                       | Status                                                                                                              |
| ---------------------------- | ----------- | -------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| GHSA-cq8v-f236-94qc (low)    | rand 0.7.3  | build-time only: `tauri-utils` → `kuchikiki` → `selectors` → `phf_codegen` | Unpatched upstream; no untrusted input is processed by this crate in voxix (it generates UI literals at build time) |
| GHSA-wrw7-89jp-8q8g (medium) | glib 0.18.5 | `gtk-rs` 0.18 via `tray-icon` / `gtk-layer-shell`                          | Fix requires the gtk-rs 0.20 ecosystem upgrade (Tauri-wide). No `VariantStrIter` usage anywhere in voxix            |

Both will be resolved by moving to the next upstream gtk-rs release; track this issue for removal once Tauri's gtk stack ships glib ≥ 0.20.
