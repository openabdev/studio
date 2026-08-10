# ADR: Remote Upgrade via GitHub — unified core+skin auto-update

- **Status:** Proposed
- **Date:** 2026-08-09
- **Author:** @brettchien
- **Reviewers:** _pending_
- **Tracking issues:** builds on [desktop-console](./desktop-console.md) (ADR-3)

> **Y-statement.** In the context of shipping the Studio desktop app to
> operators, facing the need to deliver **core + skin** updates without manual
> rebuilds, we decided a **unified auto-update off GitHub Releases**: CI
> publishes a signed per-platform bundle plus a `latest.json` manifest, and the
> **Tauri updater** checks, downloads, verifies, and installs it — **accepting
> that core and skin version together** (no independent skin hot-update this
> phase) and that **OS-level code-signing certificates are a separate,
> deferred prerequisite** (dev builds ship unsigned until they exist).

---

## 1. Context & Problem

ADR-3 gives us a desktop console (Tauri web skin over a local Rust core,
macOS + Windows). Operators run a binary; we need a way to **push updates** —
both the Rust **core** (`studio-cp` / `oab-mcp`) and the web **skin** — without
asking anyone to rebuild or re-download by hand. The delivery channel should be
**GitHub**, which already hosts the source and CI.

Because the desktop core ships *inside* the app bundle (ADR-3: local core over
stdio), a single app update carries both core and skin. The remote-service core
(for the deferred iOS/browser topology) upgrades via its own server deploy and
is out of scope here.

## 2. Decision Drivers

- **No manual rebuilds** — operators self-update from a published release.
- **Authenticity** — an update must be provably from us before it runs.
- **GitHub-native** — reuse Releases as the artifact + manifest host; no new
  infra to stand up.
- **Simplicity over skin-agility (for now)** — the interim skin will be replaced
  by SwiftUI (ADR-3); not worth a bespoke skin hot-update path yet.

## 3. Decision

### 3.1 Unified bundle, Tauri updater

Core and skin ship as **one signed bundle per platform**. The app embeds
`tauri-plugin-updater`; on launch (and on demand) it checks the manifest,
and if a newer version exists, downloads → **verifies the updater signature** →
installs → relaunches. One update moves both layers; there is no separate
skin-only channel this phase (the ADR-3 "split" option is explicitly deferred —
see §5).

### 3.2 GitHub Releases as the feed

A tagged release (`vX.Y.Z`) carries, per platform, the bundle and an updater
`latest.json` manifest:

```
 tag v0.2.0
   ├─ Studio_0.2.0_aarch64.app.tar.gz   (+ .sig)   macOS
   ├─ Studio_0.2.0_x64-setup.exe        (+ .sig)   Windows
   └─ latest.json   { version, notes, pub_date, platforms:{ …: { url, signature } } }
```

The app's updater `endpoints` point at the release's `latest.json` (GitHub
serves it at a stable `releases/latest/download/latest.json` URL).

### 3.3 Two signatures — do not conflate

| Signature | Purpose | Key / cert | Where |
|-----------|---------|------------|-------|
| **Updater signature** | proves the *update bundle* is authentic to the app | Tauri minisign keypair | private key = CI secret; public key in `tauri.conf.json` |
| **OS code-signing** | makes the *OS* trust the app (no "unidentified developer") | Apple Developer cert + notarization (macOS); Authenticode (Windows) | separate certs, **deferred** — see §6 |

The updater signature is **required** for auto-update and is cheap (a generated
keypair). OS code-signing is a separate, paid prerequisite for a clean install
experience; until it exists we ship **ad-hoc-signed dev builds** (Tauri's
default; on Apple Silicon a *truly* unsigned app will not launch at all). They
run, but first-launch is gated by a Gatekeeper prompt — right-click → Open, or
`xattr -dr com.apple.quarantine` — not a passive warning. Acceptable for
internal/operator use.

### 3.4 Release trigger & versioning

Semver in `tauri.conf.json` / `package.json`; pushing a `vX.Y.Z` **tag** triggers
a `release` workflow that builds each platform, signs the bundle with the CI
updater key, and publishes bundles + `latest.json` to the GitHub Release
(`tauri-apps/tauri-action` does the build+sign+publish in one step).

### 3.5 Channels

One **stable** channel initially (`latest.json` at the latest release). A `beta`
pre-release channel (separate manifest) is a later addition if needed.

## 4. Consequences

**Positive**
- Operators self-update; a tag push ships core+skin to everyone.
- Updates are signature-verified before they run.
- Zero new infra — GitHub Releases hosts artifacts and manifest.

**Negative / costs**
- Core and skin can't move independently (accepted; revisit with the split path).
- A clean install needs **paid OS code-signing** (Apple/Windows); until then the
  OS warns on first run.
- The updater **private key** is a release-critical secret to guard and rotate.

**Neutral**
- The remote-service core (iOS/browser topology, ADR-3 deferred) upgrades via its
  own deploy pipeline, not this app-update path.

## 5. Alternatives Considered

- **Split skin/core update (ADR-3 option B).** Skin (web assets) hot-updates
  from GitHub independent of the core. Faster skin iteration, but adds a second
  update path and the security surface of loading code outside the signed
  bundle. Deferred; not worth it for a throwaway interim skin.
- **App stores (Mac App Store / MS Store).** OS-trusted distribution + updates,
  but store review latency and sandbox constraints fit a control-plane tool
  poorly. Possible much later for reach, not for the operator tool.
- **Manual downloads.** No auto-update; rejected — defeats the purpose.

## 6. Open Questions / Deferred

- **OS code-signing.** Apple Developer account (macOS notarization) and a Windows
  code-signing certificate — provision now, or ship unsigned dev builds first?
  (Default until decided: unsigned.)
- **Distribution surface.** `openabdev/studio` is **public**, so Releases and
  bundles are public. Confirm public distribution is intended, or move to a
  private release channel.
- **Updater key custody.** Who generates and holds the minisign private key
  (CI secret), and the rotation policy.
- **Split skin update.** Revisit ADR-3 option B if skin iteration speed on the
  interim web build ever justifies it.

## 7. Appendix — pieces to drop in at console slice-2

*Not committed yet (the Tauri app lands in ADR-3 slice-2); recorded so the
pipeline is ready.*

- **`tauri.conf.json`** — `plugins.updater`:
  ```json
  { "plugins": { "updater": {
      "endpoints": ["https://github.com/openabdev/studio/releases/latest/download/latest.json"],
      "pubkey": "<minisign public key>"
  } } }
  ```
- **`.github/workflows/release.yml`** — on `push: tags: ['v*']`, matrix over
  macOS + Windows, `tauri-apps/tauri-action` with `TAURI_SIGNING_PRIVATE_KEY`
  (secret) → build, sign, publish bundles + `latest.json` to the Release.
- **Keypair** — `pnpm tauri signer generate`; public key → `tauri.conf.json`,
  private key → the `TAURI_SIGNING_PRIVATE_KEY` repo secret.
