# 贡献指南

感谢关注 Toktol。这份文档说明本仓库的开发流程与不可妥协的架构约束。

## 开发环境

- Node.js 22+
- pnpm 12（版本由根目录 `package.json` 的 `packageManager` 字段锁定）
- Rust stable —— 最低版本（MSRV）的唯一事实源是根 `Cargo.toml` 的 `rust-version`，
  CI 的 `msrv` 检查线会读取它并在该版本上编译，所以这里不再写死具体数字
- Tauri 2 的平台构建依赖（分平台见下）

```bash
pnpm install        # 安装 JS 依赖
pnpm tauri dev      # 编译 Rust 壳并打开应用窗口
```

### Linux

系统依赖清单以 `scripts/install-linux-deps.sh` 为准。CI 的检查线与发布线调用的也是同一份脚本，所以这份清单不会和工作流产生漂移：

```bash
bash scripts/install-linux-deps.sh
```

脚本需要 Debian / Ubuntu 系发行版，其余发行版请对照下面链接手动安装。

### macOS / Windows

通常无需额外系统依赖，按 [Tauri 环境准备](https://v2.tauri.app/start/prerequisites/) 核对即可——Windows 一般需要 WebView2 Runtime，macOS 需要 Xcode 命令行工具。

dev server 固定占用 21720 端口，被占用时直接失败——Tauri 配置写死了这个地址。

## 提交前必须全绿

```bash
pnpm check                  # 校验脚本 + cargo fmt --check + clippy -D warnings + tsc --noEmit
pnpm test                   # vitest
cargo test --workspace      # 三个 crate 的 Rust 测试
```

`pnpm check` 里三个校验脚本按"快 → 慢"排在最前面：`verify:deps`（当前仅告警）、
`verify:constants`、`verify:version`，几秒钟就能拦住声明层面的漂移，不必等编译。

CI 在 Ubuntu 上跑 `pnpm check` 与两套测试，另外在 Windows、macOS 上跑
`cargo test --workspace`——路径处理、回收站、文件监听都是平台敏感的，本地只测一个平台
不代表跨平台没问题。

## 架构铁律（评审会打回的那种）

1. **依赖方向**：`toktol-core` ← `toktol-gateway` ← `src-tauri`。`toktol-core` 绝不允许
   依赖网关或壳；反向依赖的 PR 会被直接拒绝。
2. **业务逻辑只能住在 `toktol-core`**。桌面壳只做窗口与命令的薄封装。
3. **单一事实源在 `toktol-core/src/paths.rs`**。数据目录、文件名、令牌前缀、localStorage
   键名都以这里的常量为准，禁止在任何地方重写 `".toktol"`、`"toktol.db"` 这类字面量。
4. **版本号三处一致**：`package.json`、`Cargo.toml` 的 `[workspace.package]`、
   `src-tauri/tauri.conf.json`。`scripts/verify-release-version.mjs` 会强制校验。
5. **profile 只写在工作区根 `Cargo.toml`**。成员 crate 里的 `[profile.*]` 会被 Cargo
   忽略并告警。
6. **依赖按阶段引入，声明必须在代码里被使用**。不要为了让"后面会用到"而提前声明——
   未使用的依赖照样付编译时间、依赖树体积和审计成本，和本项目"隐私、本地优先"的定位
   冲突。阶段 2+ 需要的依赖统一记在根 `Cargo.toml` 的注释里，届时连同版本与特性一起评估。
   `scripts/verify-declared-deps.mjs` 负责盯住这点。
7. **UI 文案只写在 `src/i18n/strings.ts`**。组件里不许出现硬编码中文，新增文案一律
   加表后引用键。理由不是"以后要搬家"，而是**防扩散**：从现在到阶段 5 做多语言之间，
   每写一个组件就多一批硬编码文案，届时再去翻所有组件代价太大。
8. **语义色不得混用**。涨跌用 `up` / `down`（数据语义，红涨绿跌），连接/成功/失败等
   状态用 `ok` / `danger`（结果语义）。借用会让同一个红色在两个场景表示相反的含义
   （涨跌里是"涨"，状态里是"出错"），用户认知打架。
9. **前端纯函数与副作用分居**。`src/lib/` 平铺区只放纯函数模块，带状态或副作用的
   hooks 一律进 `src/lib/hooks/`。

## 测试准则

- **断言行为，不复述声明。** 判据是"断言对象有没有实现体"：函数/方法有实现体、会漂移
  （可能被写成硬编码），值得测；`pub const X = "字面量"` 与 `pub use a::b;` 是**声明即
  定义**，断言 `X == "字面量"` 只是把同一句话写两遍——改常量的人会顺手改测试，
  它给的是虚假的安全感。评审时按这条判据打回。
- **常量测不变量，不测字面量。** 钉住下游真正依赖的规则（路径名不含分隔符、标识符合法、
  令牌前缀能进 HTTP 头），而不是"常量等于哪个字符串"。
- **跨语言一致性靠脚本，不靠两侧各自单测**——单测读不到对方语言的源码。
  契约由 `scripts/verify-constants-parity.mjs` 守。
- **删掉同义反复的测试不要心疼**。测试数量不是质量指标，占比里掺水的部分会掩盖真正的
  覆盖缺口。

## 隐私红线（产品约束，不是建议）

- 只读扫描：绝不修改 AI 工具的日志文件；删除会话走系统回收站，绝不硬删除。
  会话存在工具自有 SQLite 库里的（opencode、zcode 轮转会话），先把会话行导出为
  JSON 包移入回收站，入桶成功后才对工具库删行。
- 绝不读取 API key、token 或环境变量。
- 未定价的模型绝不估价，显示"未知"而不是编造数字。
- 所有数据只留在 `~/.toktol/`，不引入任何网络上报。

涉及上述任何一条的功能设计，请先开 issue 讨论。

## 提交规范

- 一个 commit 只做一件事，提交信息说明"为什么"而不仅是"改了什么"。
- 发布由 tag 驱动：`git tag v0.1.0 && git push origin v0.1.0`，CI 会校验版本一致性并
  构建三平台 Draft Release，人工确认后才正式发布。发布正文取自 CHANGELOG 对应
  版本的双语段落（`scripts/extract-release-notes.mjs` 提取）——打 tag 前段落必须
  已写好，缺失即拦截；段落里的链接一律绝对 URL（Release 页不解析相对路径）。
- 新增依赖需要在 PR 里说明理由——尤其是会给发布包增重的依赖（`reqwest` 会拖入 TLS
  整棵树）。声明依赖的同一个 PR 里必须有真正使用它的代码，否则 `verify:deps` 会报出来。

## 报告问题

请附上：操作系统与版本、Toktol 版本号、复现步骤、预期与实际行为。日志位于
`~/.toktol/`，粘贴前请自行检查是否含敏感信息。
