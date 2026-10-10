//! `toktol-core` —— Toktol 的领域核心库：业务逻辑只住在这里。
//! 架构铁律与依赖方向（core ← gateway ← 壳）见 CONTRIBUTING.md；
//! 隐私红线（只读扫描、回收站删除、未定价绝不估价）见 README。
//! 当前阶段 2：`storage` 已实现，其余模块占位待建。

#![deny(missing_docs)]

pub mod adapter;
pub mod error;
pub mod fsutil;
pub mod model;
pub mod paths;

/// 把当前线程降为后台优先级（Windows：THREAD_MODE_BACKGROUND_BEGIN，CPU 与
/// I/O 一并降；其余平台无事可做）。转录索引建站线程用它实现"机器忙时自动
/// 让路、空闲时全速"。
pub fn set_thread_background_priority() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
        };
        // 返回值忽略：降优先级失败只影响体验，不影响正确性。
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
        }
    }
    #[cfg(not(windows))]
    {
        // 非 Windows 暂无等价实现：不加区别。
    }
}
pub mod pricing;
pub mod scan;
pub mod sessions;
pub mod storage;
pub mod toolconfig;

/// 适配器统一接口：外部调用方从这里拿契约与已注册的适配器，不深入子模块。
pub use adapter::{Adapter, adapters};

/// 产品版本，取自 Cargo 单一事实源；桌面壳的 `app_version` 命令原样返回它。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
