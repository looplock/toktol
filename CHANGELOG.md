# Changelog

All notable changes to this project will be documented in this file.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). One line per
user-visible change; design rationale lives in commit messages and docs/.

## [0.1.0] - 2026-10-08

### Added

- Usage & cost dashboard for 9 AI coding tools (Claude Code, Codex, OpenCode, ZCode, CodeBuddy, WorkBuddy, Grok, Pi, DSH): read-only scanning of local session logs into a local SQLite database.
- Local AI API gateway: reverse proxy with OpenAI/Gemini protocol translation, metering, config hot-reload, and token issuance.
- Overview dashboard: configurable cards over live usage — trend, breakdown, heatmap, efficiency — honoring the top filters and disabled tools, with local-timezone buckets (hour/day/month).
- Sessions page: filterable list; full transcripts with paged byte-range reading (multi-GB sessions open instantly, jump-to-turn), image attachments, and collapsed thinking blocks.
- Config page (per-tool connection overview) and Pricing page (price editing, models.dev catalog sync, price-set copy between models, rename/merge with price fill).
- Session deletion to the OS trash for every tool, including DB-backed tools (rows exported to a JSON bundle trashed first); deleted sessions are tombstoned.
- Derived per-request durations for claude-code, codebuddy, workbuddy, pi, and codex; grok request-level usage split from turn aggregates, anchored to tool-call events.
- Scan loop in the Rust shell: system tray with low-power mode, autostart, and scan feedback toasts.
- Light/dark theme, accent colors, zh/en UI, number-unit display options, and a chart-animation preference.
- Cost-estimation policy: missing cache-read/write prices fall back to the input price, and unpriced models report "unknown" — never an invented number.
- Resilient scanning: oversize logs are streamed instead of skipped; zcode usage reads from its own app database.
- Dev experience: dependency builds compiled with O2 (~30x faster first scan).
