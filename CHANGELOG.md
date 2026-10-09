# Changelog

All notable changes to this project will be documented in this file.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). One line per
user-visible change; design rationale lives in commit messages and docs/.

每个版本段落双语（`### Added` + `### 新增`），并自带下载产物表与签名警告——
它就是发布页正文的**单一事实源**：release 工作流用
`scripts/extract-release-notes.mjs` 原文提取该段落作为 GitHub Release 正文
（对应版本段落缺失即拦截发布）。段落里的链接一律写绝对 URL：GitHub Release
页不解析仓库相对路径。

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

### 新增

- 面向 9 个 AI 编程工具的用量与成本仪表盘（Claude Code、Codex、OpenCode、ZCode、CodeBuddy、WorkBuddy、Grok、Pi、DSH）：只读扫描本地会话日志，汇总进本地 SQLite 数据库。
- 本地 AI API 网关：反向代理 + OpenAI/Gemini 协议转换 + 计量，支持配置热重载与令牌签发。
- 概览仪表盘：可自由布局的卡片聚合实时用量——趋势、构成、热力图、效率——遵循顶部筛选与已停用工具设置，本地时区分桶（时/天/月）。
- 会话页：可筛选列表；完整转录支持分页字节区间读取（多 GB 会话秒开、跳转回合）、图片附件、思考块折叠。
- 配置页（各工具连接概览）与定价页（价格编辑、models.dev 目录同步、跨模型整组复制价格、改名/合并并回填价格）。
- 全工具会话删除均走系统回收站，含数据库类工具（先将会话行导出为 JSON 包入桶，成功后才删行）；已删会话做墓碑标记防止复活。
- 为 claude-code、codebuddy、workbuddy、pi、codex 推导按请求时长；grok 回合聚合用量按真实请求拆分，锚定 tool-call 事件时刻。
- 扫描循环驻留 Rust 壳：系统托盘、低功耗模式、开机自启与扫描反馈气泡。
- 亮/暗主题、强调色、中英界面、数字单位显示选项与图表动画偏好。
- 成本估算口径：缺失的缓存读/写价格回退到输入价；未定价模型显示"未知"，绝不编造数字。
- 韧性扫描：超限日志流式解析而非跳过；zcode 用量改从其自有应用数据库读取。
- 开发体验：依赖以 O2 编译（首扫提速约 30 倍）。

### Downloads / 下载

| Platform / 平台 | Asset / 产物 |
|---|---|
| Windows | `Toktol_0.1.0_x64-setup.exe` |
| macOS (Apple Silicon) | `Toktol_0.1.0_aarch64.dmg` |
| Linux | `Toktol_0.1.0_amd64.AppImage` / `Toktol_0.1.0_amd64.deb` |

⚠️ Installers are **not code-signed yet**: Windows SmartScreen and macOS Gatekeeper show a
one-time security prompt on first launch — expected, not tampering. Verify the SHA-256
digest listed next to each asset. Details: [docs/install.md](https://github.com/looplock/toktol/blob/main/docs/install.md).

⚠️ 安装包**暂未代码签名**：Windows SmartScreen 与 macOS Gatekeeper 首次运行会出示一次性
安全提示，属预期行为而非安装包被篡改。安装前请核对各产物旁的 SHA-256 摘要，
详见 [docs/install.md](https://github.com/looplock/toktol/blob/main/docs/install.md)。

**Nothing ever leaves your machine** — no account, no telemetry, no cloud sync. All data
stays in `~/.toktol/`.
**数据不出本机** —— 没有账号，没有遥测，没有云端同步。所有数据只留在 `~/.toktol/`。
