<div align="center">

# Toktol

<strong>本地优先的 AI 编程工具用量与成本仪表盘。</strong>

<p>
  <a href="README.md">English</a>
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

<!-- TODO: 首个 release 产物构建出来后，在这里补仪表盘截图。 -->

## 背景

你在用的每个 AI 编程工具其实都在本机写会话日志——但没有一个会告诉你，一个月写下来到底
花了多少钱。Toktol 在本地补上这块：扫描这些日志，把 token 用量与估算成本汇总进本地
SQLite 并可视化。随附的第二个功能是**本地 AI API 网关**（反向代理 + 协议转换 + 计量），
网关流量同样计入统计。

支持的工具：Claude Code、Codex、OpenCode、ZCode、CodeBuddy、WorkBuddy、Grok、Pi、DSH。

## 隐私红线

Toktol 只读取工具日志、绝不修改，删除会话仅走系统回收站，绝不硬删除；绝不读取 API
key、token 或环境变量。所有数据只留在 `~/.toktol/`；没有已知价格的模型显示"未知"，
绝不编造数字——没有账号，没有遥测，没有云端同步。

## 安装

从 [Releases](../../releases) 页下载对应平台的安装包。发布产物**未做代码签名**，
Windows SmartScreen 与 macOS Gatekeeper 首次运行会出示一次性安全提示——属预期行为，
不是安装包被篡改。安装前请核对 release 产物自带的 SHA-256 摘要，或从源码自建。
详细步骤见 [docs/install.md](docs/install.md)。

## 使用

- **概览仪表盘**——token 用量与估算成本一屏总览，支持时间 / 工具 / 项目 / 模型筛选，
  卡片布局可自由调整。
- **会话**——每个会话的完整转录：可搜索、可分页，多 GB 日志也能秒开。
- **网关**——把本地工具指到本机端点，管理上游与映射，经过的每个请求都被计量。
- **常驻后台**——后台扫描循环 + 系统托盘，支持低功耗模式与开机自启。

## 开发

环境要求：Node.js 22+、pnpm 12（由 `packageManager` 锁定）、Rust stable（MSRV 见根
`Cargo.toml` 的 `rust-version` 字段），以及
[Tauri 2 平台构建依赖](https://tauri.app/start/prerequisites/)。

```bash
pnpm install          # 安装 JS 依赖
pnpm tauri dev        # 编译 Rust 壳并打开应用窗口
```

dev server 固定占用 21720 端口，被占用时直接失败——Tauri 配置写死了这个地址。

## 技术栈

| 层 | 选型 |
|---|---|
| 桌面壳 | Tauri 2（`single-instance`、`autostart`、`opener` 插件） |
| 系统语言 | Rust（stable，edition 2024）——`toktol-core`（领域）+ `toktol-gateway`（axum + tokio） |
| 前端 | React 19 + TypeScript（strict）+ Vite 7，包管理 pnpm |
| 样式 | Tailwind CSS 4 |
| 图表 | ECharts 6，配自写 React hook |
| 存储 | SQLite（WAL 模式），位于 `~/.toktol/` |

## 仓库结构

```
toktol/
├─ crates/toktol-core/      # 领域逻辑——业务逻辑只能住在这里
├─ crates/toktol-gateway/   # 本地 API 网关
├─ src-tauri/               # 桌面壳——只做窗口与命令的薄封装
├─ src/                     # React 前端
├─ scripts/                 # 发布工具
└─ .github/workflows/       # CI（检查线）、Release（tag → 三平台 → Draft Release）与依赖审计
```

依赖方向是硬约束：

```
toktol-core  ←  toktol-gateway  ←  src-tauri
```

`toktol-core` 绝不允许依赖网关或壳；壳里写业务逻辑会在评审时被打回。

## 参与贡献

架构铁律、质量门禁、版本管理与发布流程见 [CONTRIBUTING.md](CONTRIBUTING.md)；
安全话题遵循 [SECURITY.md](SECURITY.md)。

## 项目文档

- [安装发布版](docs/install.md) —— 未签名提示的处理、SHA256 核验与源码自建
- [更新日志](CHANGELOG.md) —— 每个版本改了什么
- [安全策略](SECURITY.md) —— 如何报告漏洞与隐私红线违规

## 许可证

MIT —— 见 [LICENSE](LICENSE)。
