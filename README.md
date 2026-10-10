<div align="center">

# Toktol

<strong>A local-first usage &amp; cost dashboard for AI coding tools.</strong>

<p>
  <a href="README.zh-CN.md">简体中文</a>
</p>

<p>
  <a href="https://github.com/looplock/toktol/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/looplock/toktol/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/looplock/toktol/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/looplock/toktol"></a>
  <a href="https://github.com/looplock/toktol/releases/latest"><img alt="Downloads" src="https://img.shields.io/github/downloads/looplock/toktol/total"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
</p>
<p>
  <a href="https://tauri.app/"><img alt="Tauri 2" src="https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white"></a>
  <a href="https://react.dev/"><img alt="React 19" src="https://img.shields.io/badge/React-19-61DAFB?logo=react&logoColor=white"></a>
  <a href="https://www.rust-lang.org/"><img alt="Rust backend" src="https://img.shields.io/badge/Rust-backend-000000?logo=rust&logoColor=white"></a>
  <a href="https://www.typescriptlang.org/"><img alt="TypeScript frontend" src="https://img.shields.io/badge/TypeScript-frontend-3178C6?logo=typescript&logoColor=white"></a>
</p>

</div>

## Background

Every AI coding tool you use already writes session logs on your machine — but none of them
tell you what a month of coding actually cost. Toktol fills that gap locally: it scans those
logs, aggregates token usage and estimated cost into a local SQLite database, and visualizes
it. A second feature ships alongside: a **local AI API gateway** (reverse proxy + protocol
translation + metering), so traffic through the gateway is counted too.

Supported tools: Claude Code, Codex, OpenCode, ZCode, CodeBuddy, WorkBuddy, Grok, Pi, DSH.

## Privacy guarantees

Toktol reads tool logs read-only and never modifies them; deleting a session goes to the
OS trash, never a hard delete. It never reads API keys, tokens, or environment variables.
Unpriced models show "unknown" instead of an invented number, and all data stays in
`~/.toktol/` — no account, no telemetry, no cloud sync.

## Install

Grab an installer for your platform from the [Releases](../../releases) page. The bundles are
**not code-signed yet**, so Windows SmartScreen and macOS Gatekeeper show a one-time security
prompt on first launch — expected, not tampering. Verify the SHA-256 digest shown on each
release asset, or build from source. Walkthrough: [docs/install.md](docs/install.md) (Chinese).

## Usage

- **Overview dashboard** — token usage and estimated cost at a glance, with time / tool /
  project / model filters and an adjustable card layout.
- **Sessions** — every session with its full transcript: searchable, paged, multi-GB logs
  included.
- **Gateway** — point local tools at the local endpoint, manage upstreams and mappings, and
  meter every request that passes through.
- **Always on duty** — background scan loop with a system tray, low-power mode, and autostart.

## Development

Requirements: Node.js 22+, pnpm 12 (pinned via `packageManager`), Rust stable (MSRV declared
as `rust-version` in the root `Cargo.toml`), and the
[Tauri 2 platform prerequisites](https://tauri.app/start/prerequisites/).

```bash
pnpm install          # JS dependencies
pnpm tauri dev        # build the Rust shell and open the app window
```

The dev server occupies port 21720 and fails loudly when it is taken — the Tauri config points
at it explicitly.

## Tech stack

| Layer | Choice |
|---|---|
| Desktop shell | Tauri 2 (`single-instance`, `autostart`, `opener` plugins) |
| Systems language | Rust (stable, edition 2024) — `toktol-core` (domain) + `toktol-gateway` (axum + tokio) |
| Frontend | React 19 + TypeScript (strict) + Vite 7, pnpm |
| Styling | Tailwind CSS 4 |
| Charts | ECharts 6, driven by a hand-written React hook |
| Storage | SQLite (WAL mode) under `~/.toktol/` |

## Repository layout

```
toktol/
├─ crates/toktol-core/      # domain logic — the only place business logic may live
├─ crates/toktol-gateway/   # local API gateway
├─ src-tauri/               # desktop shell — thin window/command wrapper only
├─ src/                     # React frontend
├─ scripts/                 # release tooling
└─ .github/workflows/       # CI (checks), Release (tag -> 3 platforms -> draft release), dependency audit
```

Dependency direction is enforced by design:

```
toktol-core  ←  toktol-gateway  ←  src-tauri
```

`toktol-core` must never depend on the gateway or the shell. Business logic in the shell crate is
rejected in review.

## Contributing

Architecture rules, quality gates, versioning, and the release process live in
[CONTRIBUTING.md](CONTRIBUTING.md). Security topics follow [SECURITY.md](SECURITY.md).

## Project documents

- [Installing releases](docs/install.md) — unsigned-installer walkthrough, SHA256
  verification, and building from source (Chinese)
- [Changelog](CHANGELOG.md) — what changed in each release
- [Security](SECURITY.md) — how to report vulnerabilities and privacy-red-line violations

## License

MIT — see [LICENSE](LICENSE).
