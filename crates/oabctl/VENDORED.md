# Vendored: oabctl

This crate is vendored from **[openabdev/openab](https://github.com/openabdev/openab)**
(`operator/`), which is licensed **MIT** (Copyright (c) 2026 openabdev).

- Source: `openabdev/openab` @ `d64c678f0b5e4b26f52d5272b0c6743c4207a1b9`
- Vendored: 2026-08-08

We copied it (rather than depending across repos) so Studio can build the
control-plane actions and an MCP surface on top of it in one place. Upstream
changes are **not** auto-synced; re-vendor deliberately and record the new sha
here.

MIT license text: see the repo root `LICENSE` (Studio is also MIT) and the
upstream `openabdev/openab` `LICENSE`.

## Studio-local additions (not from upstream)

To keep the diff against upstream auditable, Studio-specific changes are
additive and listed here:

- **`src/studio_api.rs`** — a programmatic surface (`parse_manifests`, `scale`,
  `delete`) so `studio-cp` / `oab-mcp` call a library API instead of shelling
  out. Thin wrappers over existing internals; no upstream behaviour changed.
- **`src/lib.rs`** — one line: `pub mod studio_api;`.
