//! 壳偏好 `~/.toktol/config.json` 的读写：托盘开关、低功耗模式。只有 Rust 壳
//! 能在启动早期（前端 localStorage 尚不可达）就需要的东西才放这里；纯界面
//! 偏好归前端 localStorage（划界见 toktol-core 的 paths.rs）。
//!
//! 写方只有本模块，读改写全程持同一把进程内锁；写失败不致命（托盘/低功耗
//! 丢一次持久化而已），按 log 输出后继续。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// 托盘是否常驻。缺省 = 开：托盘是产品的主要唤起方式。
pub const TRAY_DEFAULT: bool = true;
/// 低功耗模式（暂停自动扫描，只留手动触发）。缺省 = 关。
pub const LOW_POWER_DEFAULT: bool = false;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellConfig {
    #[serde(default = "default_true", rename = "tray")]
    pub tray: bool,
    /// 低功耗模式：主窗口销毁、仅托盘常驻；**扫描照常**（常驻循环与窗口无关）。
    /// 仅会话内生效——启动一律重置为关，这里落盘只是顺带清残留，启动方不读。
    #[serde(default, rename = "lowPower")]
    pub low_power: bool,
    /// 被禁用的工具 id 清单（前端 localStorage 的镜像）：扫描循环每轮读取，
    /// 窗口销毁后前端传不了参，清单必须由壳自己持有。前端每次变更即同步。
    #[serde(default, rename = "disabledTools")]
    pub disabled_tools: Vec<String>,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            tray: TRAY_DEFAULT,
            low_power: LOW_POWER_DEFAULT,
            disabled_tools: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    TRAY_DEFAULT
}

impl ShellConfig {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            // 文件不存在或字段缺失按缺省；解析失败同样回退缺省——损坏的壳配置
            // 不值得拦住启动，写回时会整体覆盖修复。
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// 落盘走原子写（temp + rename）：写一半崩溃留下的是半份 JSON，下次启动
    /// 虽能按缺省修复，但托盘开关会静默回跳——能防住的不该靠"能修复"兜底。
    pub fn store(&self, path: &Path) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = toktol_core::fsutil::write_file_atomic(path, &text);
        }
    }
}

/// 进程内的壳配置状态：命令读改的单一入口，落盘在持锁期间完成。
/// 自身廉价克隆（内部 Arc）：manage 进 tauri 的类型是 `ShellConfigState` 本身，
/// 而扫描循环的闭包还要持一份——用 `manage(Arc<Self>)` 的话，`state::<Self>()`
/// 查的是裸类型，永远查不到（启动即 panic 的教训）。
#[derive(Clone, Default)]
pub struct ShellConfigState {
    inner: Arc<ShellConfigInner>,
}

#[derive(Default)]
struct ShellConfigInner {
    path: Mutex<Option<PathBuf>>,
    config: Mutex<ShellConfig>,
}

impl ShellConfigState {
    pub fn new(path: Option<PathBuf>, config: ShellConfig) -> Self {
        Self {
            inner: Arc::new(ShellConfigInner {
                path: Mutex::new(path),
                config: Mutex::new(config),
            }),
        }
    }

    pub fn get(&self) -> ShellConfig {
        self.inner.config.lock().expect("壳配置锁中毒").clone()
    }

    pub fn update(&self, apply: impl FnOnce(&mut ShellConfig)) -> ShellConfig {
        let mut config = self.inner.config.lock().expect("壳配置锁中毒");
        apply(&mut config);
        if let Some(path) = self.inner.path.lock().expect("壳配置路径锁中毒").as_ref() {
            config.store(path);
        }
        config.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "toktol-shell-config-{}-{tag}.json",
            std::process::id()
        ))
    }

    #[test]
    fn missing_and_corrupt_files_fall_back_to_defaults() {
        assert_eq!(
            ShellConfig::load(&temp_path("no-such-file")),
            ShellConfig::default()
        );
        let path = temp_path("corrupt");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(ShellConfig::load(&path), ShellConfig::default());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn partial_config_keeps_defaults_for_missing_fields() {
        let path = temp_path("partial");
        std::fs::write(&path, r#"{"tray": false}"#).unwrap();
        let config = ShellConfig::load(&path);
        assert!(!config.tray);
        assert!(!config.low_power, "缺省字段取缺省值");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn update_persists_to_disk() {
        let path = temp_path("persist");
        let state = ShellConfigState::new(Some(path.clone()), ShellConfig::default());
        let config = state.update(|c| c.low_power = true);
        assert!(config.low_power);
        assert!(ShellConfig::load(&path).low_power, "变更落盘");
        std::fs::remove_file(&path).unwrap();
    }
}
