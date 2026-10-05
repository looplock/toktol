//! Toktol 桌面壳的进程入口。
//!
//! 这里刻意保持极薄：只负责把控制权交给 [`toktol_lib::run`]，
//! 所有窗口/插件/命令的组装都在 lib.rs，便于后续阶段写集成测试。

// Windows 上 release 构建不弹出伴随的控制台窗口；debug 构建保留控制台便于看日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    toktol_lib::run()
}
