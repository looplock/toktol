# AGENTS.md

面向在本仓库工作的 AI 代理（与人类贡献者同样适用）。架构与流程约束见 [CONTRIBUTING.md](./CONTRIBUTING.md)。

## 注释纪律

只写代码里看不出来的信息，且尽量短。判据：删掉这条注释，读者会不会走弯路？会才留。

该写：

- **为什么**：决策理由、约束来源、看似多余的代码为何存在
  （如"静默跳过：写不进去也不该影响本次会话"）。
- **非显而易见的契约**：环绕语义、偏移量不限 ±1 这类签名看不出的约定。
- **陷阱**：不改不知道会炸的地方。

不该写：

- 复述签名：`/** 页面标题。 */` 这类。
- 设计论文与过程记录：压成一两行，其余交给 commit message 和 CONTRIBUTING。

文件头 ≤3 行，能用一行就不用两行。注释和代码二选一能说清时，优先改代码（命名、拆函数）。

测试名同理：断言行为，不写小作文。

## 提交信息

一律英文，与改动面向哪种语言无关——`git log` 是唯一贯穿 Rust / TS / 文档的变更记录，
混用语言会让 `--grep` 和 release notes 都得处理两套。

格式为 Conventional Commits：`type(scope): 祈使句摘要`，无句号。

- type 固定词表：`feat` / `fix` / `perf` / `refactor` / `docs` / `test` / `ci` / `build` / `chore`；
  分别对应 CHANGELOG 的 Added / Fixed / Changed，起草 release notes 按前缀筛即可。
- scope 用仓库现有模块名，不发明新词：`core`、`scan`、`adapter/<tool>`、`gateway`、
  `shell`、`dashboard`、`sessions`、`pricing`、`i18n`、`deps`。
- 破坏性变更在 `:` 前加 `!`（如 `feat(gateway)!:`）。
- 正文只在首行说不清为什么时才写，一到两句；不复述 diff。
- 改动本身是中文内容（UI 文案、注释）时不翻译内容，只说改了什么、为什么改。

示例：`feat(adapter/grok): split turn usage into request-level rows`、
`fix(core): clamp overview usage time to the selected range`。

## 本地信息

进入仓库的示例与夹具一律合成值：路径、用户名、机器名、会话 id、时间戳，
都不取自本机真实数据。判据与注释同款：换成合成值后读者和测试都不走弯路，就该换。

- 示例路径自造，且与文中规则自洽（如 `d-Work-Demo-App` → `D:\Work\Demo\App`）。
- 测试夹具用合成 id 与时间戳，不从真实会话取数。
- 提交信息只说改动本身，不带本机目录布局、机器名等环境细节。
