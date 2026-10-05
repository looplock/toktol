//! `toktol-gateway` —— 本地 AI API 网关：反向代理 + 协议转换 + 计量，
//! 让本地工具的请求统一经过网关，把网关流量也纳入统计。
//! 依赖方向：依赖 [`toktol_core`]（允许），绝不依赖 `src-tauri`。
//!
//! 陷阱：`toktol-core` 这个依赖不能从 Cargo.toml 删——它是
//! scripts/verify-declared-deps.mjs 白名单里防反向依赖的循环检查，删掉等于撤掉唯一的
//! 机器检查。同理，不要再把 `paths` 常量 re-export 进来或写"断言常量等于自己"的测试。
//! 入口：[`proxy::serve`]；配置契约见 [`config`]，转换见 [`translate`]，落库见 [`metering`]。

pub mod config;
pub mod metering;
pub mod proxy;
pub mod translate;
