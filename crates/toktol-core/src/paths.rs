//! 数据目录与文件名的**单一事实源**：任何"某个文件放在哪"都必须引用这里的常量/函数，
//! 禁止在别处重写 `".toktol"`、`"toktol.db"` 等字面量；跨语言镜像由 verify:constants 守。

use std::path::PathBuf;

/// 产品显示名。
pub const PRODUCT_NAME: &str = "Toktol";
/// 二进制名（同时也是 crate `src-tauri` 的包名）。
pub const BINARY_NAME: &str = "toktol";
/// 应用标识：Tauri `identifier` 与平台相关目录命名共用。
pub const APP_IDENTIFIER: &str = "app.toktol.desktop";

/// 数据根目录名，位于用户主目录下（`~/.toktol/`）。
pub const DATA_DIR_NAME: &str = ".toktol";

/// 唯一的 SQLite 库。曾规划把网关计量拆成独立 `gateway.db`，已废弃，理由见 [`main_db_path`]。
pub const MAIN_DB_FILE: &str = "toktol.db";

/// 网关配置（**用户手写**、热重载）；与 [`SHELL_CONFIG_FILE`] 必须分文件，原因见后者。
pub const GATEWAY_CONFIG_FILE: &str = "gateway.json";

/// 壳偏好（程序自动写）：窗口几何等只有 Rust 壳能感知的东西。
///
/// 与 [`GATEWAY_CONFIG_FILE`] 分文件的原因是**写方不同**：合并的话，程序自动写回
/// 窗口几何时会静默冲掉用户手写的网关上游与路由。曾试过合并，因这个双写问题退回。
pub const SHELL_CONFIG_FILE: &str = "config.json";

/// 用户定价覆盖（**用户手写**的 JSON，让人能用文本编辑器直接改，故不进数据库）。
pub const PRICING_OVERRIDES_FILE: &str = "pricing-overrides.json";

/// 删除数据库类来源会话时的导出包暂存目录：bundle 写在这里、随即移入系统
/// 回收站（顺带离开本目录）。
pub const TRASH_STAGING_DIR: &str = "trash-staging";

/// 网关访问令牌前缀，签发与校验两端都必须引用它，不得复制字面量。
pub const GATEWAY_TOKEN_PREFIX: &str = "sk-toktol-";

/// localStorage 键：界面语言与主题。**划界**：外观类偏好归前端 localStorage，
/// 不进 [`SHELL_CONFIG_FILE`]——前端首屏要同步读到，走 IPC 往返首屏会闪。
pub const LS_KEY_LOCALE: &str = "toktol-locale";
/// localStorage 键：明暗偏好（`light` / `dark` / `system`）。归属同 [`LS_KEY_LOCALE`]。
/// 主题只有明暗这一维，
/// 一个键就够；`system` 表示跟 OS 走，实际落到的明暗运行时算。
pub const LS_KEY_THEME: &str = "toktol-theme";
/// localStorage 键：强调色预设（`indigo` / `blue` / …，见前端 theme.ts 的 ACCENT_IDS）。
/// 归属同 [`LS_KEY_LOCALE`]（前端外观偏好），Rust 侧只存键名做跨语言镜像。
pub const LS_KEY_ACCENT: &str = "toktol-accent";
/// localStorage 键：**被禁用**的工具 id 列表（JSON 数组）。缺省 = 全部启用——
/// 新工具加入后默认可用；存"禁用"而不是"启用"，恢复默认只需清键。
/// 归属同 [`LS_KEY_LOCALE`]（前端偏好）；扫描与查询过滤由前端把清单随 IPC 传入。
pub const LS_KEY_TOOLS_DISABLED: &str = "toktol-tools-disabled";
/// localStorage 键：明细页 Tokens 列的输入口径（`input` / `inputWithCacheRead`，
/// 见前端 inputScope.ts）。归属同 [`LS_KEY_LOCALE`]（前端偏好），Rust 侧只存键名做跨语言镜像。
pub const LS_KEY_INPUT_SCOPE: &str = "toktol-input-scope";
/// localStorage 键：token 数量的单位制（`chinese` / `english` / `plain`，
/// 见前端 numberUnit.ts）。归属同 [`LS_KEY_LOCALE`]（前端偏好），Rust 侧只存键名做跨语言镜像。
pub const LS_KEY_NUMBER_UNIT: &str = "toktol-number-unit";

/// 解析用户主目录：直接用 [`std::env::home_dir`]（1.87 起解除弃用，MSRV 因此锚定 1.87）。
///
/// 陷阱：不要手写 `USERPROFILE` 替代——标准库有两层兜底（Unix 查 `/etc/passwd`、
/// Windows 调 `GetUserProfileDirectory`）而 `var_os` 没有；且空的 `USERPROFILE` 会被
/// `var_os` 当有效值，拼出相对路径 `.toktol`，数据落到进程 CWD 而非 `~/.toktol/`，
/// 直接违反隐私承诺（GUI 的 CWD 不可控）。
pub fn home_dir() -> Option<PathBuf> {
    std::env::home_dir()
}

/// 数据目录 `~/.toktol/`。
pub fn data_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join(DATA_DIR_NAME))
}

/// 唯一的库文件完整路径 `~/.toktol/toktol.db`。
///
/// **为什么只有一个库**：网关计量要在一次事务里写请求记录、读定价、落回成本，拆库就
/// 得靠 ATTACH，而 WAL 下跨 ATTACH 库的事务只对单文件原子（见
/// <https://www.sqlite.org/lang_attach.html>），崩溃可能让请求记录和成本对不上——
/// 对成本统计工具不可修复。写冲突不靠分库防：单实例插件 + WAL 已足够。
pub fn main_db_path() -> Option<PathBuf> {
    data_dir().map(|dir| dir.join(MAIN_DB_FILE))
}

/// 网关配置完整路径 `~/.toktol/gateway.json`。
pub fn gateway_config_path() -> Option<PathBuf> {
    data_dir().map(|dir| dir.join(GATEWAY_CONFIG_FILE))
}

/// 壳偏好完整路径 `~/.toktol/config.json`。
pub fn shell_config_path() -> Option<PathBuf> {
    data_dir().map(|dir| dir.join(SHELL_CONFIG_FILE))
}

/// 用户定价覆盖完整路径 `~/.toktol/pricing-overrides.json`。
pub fn pricing_overrides_path() -> Option<PathBuf> {
    data_dir().map(|dir| dir.join(PRICING_OVERRIDES_FILE))
}

/// 回收站暂存目录完整路径 `~/.toktol/trash-staging/`。
pub fn trash_staging_dir() -> Option<PathBuf> {
    data_dir().map(|dir| dir.join(TRASH_STAGING_DIR))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 常量是"声明即定义"，钉字面量只是复述；跨语言漂移由 scripts/verify-constants-parity.mjs 守。
    // 这里该钉的是**下游真正依赖的规则**——坏了会出事、但看代码不容易发现的约束。

    /// 各文件名必须是**单个路径分量**：名字里出现分隔符会让 join 写到 `~/.toktol/` 之外。
    #[test]
    fn file_names_are_single_path_components() {
        for name in [
            DATA_DIR_NAME,
            MAIN_DB_FILE,
            GATEWAY_CONFIG_FILE,
            SHELL_CONFIG_FILE,
            PRICING_OVERRIDES_FILE,
            TRASH_STAGING_DIR,
        ] {
            assert!(!name.is_empty(), "文件名不能为空");
            assert_eq!(name.trim(), name, "{name:?} 首尾带空白");
            assert!(
                !name.contains('/') && !name.contains('\\'),
                "{name:?} 含路径分隔符，join 之后会跑到预期之外的路径"
            );
        }
    }

    /// 必须是小写 reverse-DNS（只允许小写字母、数字、点，不以点开头或结尾）：
    /// 同时用作 Tauri `identifier` 与平台相关目录名。
    #[test]
    fn app_identifier_is_reverse_dns() {
        assert!(
            APP_IDENTIFIER.contains('.'),
            "{APP_IDENTIFIER:?} 不是 reverse-DNS 形状"
        );
        assert!(
            APP_IDENTIFIER
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.'),
            "{APP_IDENTIFIER:?} 含非法字符（只允许小写字母、数字、点）"
        );
        assert!(
            !APP_IDENTIFIER.starts_with('.') && !APP_IDENTIFIER.ends_with('.'),
            "{APP_IDENTIFIER:?} 不能以点开头或结尾"
        );
    }

    /// 令牌前缀会被拼进 HTTP `Authorization` 头：必须 `sk-` 开头、`-` 结尾（接随机段）、
    /// 纯 ASCII、无空白。
    #[test]
    fn gateway_token_prefix_is_header_safe() {
        assert!(
            GATEWAY_TOKEN_PREFIX.starts_with("sk-"),
            "{GATEWAY_TOKEN_PREFIX:?} 缺少 sk- 方案前缀"
        );
        assert!(
            GATEWAY_TOKEN_PREFIX.ends_with('-'),
            "{GATEWAY_TOKEN_PREFIX:?} 必须以 - 结尾才能直接拼接随机段"
        );
        assert!(
            GATEWAY_TOKEN_PREFIX.is_ascii(),
            "{GATEWAY_TOKEN_PREFIX:?} 必须是纯 ASCII"
        );
        assert!(
            !GATEWAY_TOKEN_PREFIX.chars().any(char::is_whitespace),
            "{GATEWAY_TOKEN_PREFIX:?} 含空白，会破坏 HTTP 头解析"
        );
    }

    /// 显示名非空且无首尾空白；localStorage 键还不得含空白（进存储层键名）。
    #[test]
    fn display_names_and_storage_keys_are_clean() {
        for name in [PRODUCT_NAME, BINARY_NAME] {
            assert!(!name.is_empty(), "显示名不能为空");
            assert_eq!(name.trim(), name, "{name:?} 首尾带空白");
        }

        for key in [
            LS_KEY_LOCALE,
            LS_KEY_THEME,
            LS_KEY_ACCENT,
            LS_KEY_TOOLS_DISABLED,
            LS_KEY_INPUT_SCOPE,
            LS_KEY_NUMBER_UNIT,
        ] {
            assert!(!key.is_empty(), "localStorage 键不能为空");
            assert!(!key.chars().any(char::is_whitespace), "{key:?} 含空白");
        }
    }

    /// 断言**组合关系**（`data_dir()` + 常量），不硬编码文件名字面量——写死字面量
    /// 就退化成复述常量定义，实现改了测试也照样过。
    #[test]
    fn path_derivation_composes_data_dir_with_constants() {
        let dir = data_dir().expect("测试环境应能解析主目录");
        let home = home_dir().expect("测试环境应能解析主目录");

        assert_eq!(dir, home.join(DATA_DIR_NAME));
        assert_eq!(main_db_path().unwrap(), dir.join(MAIN_DB_FILE));
        assert_eq!(trash_staging_dir().unwrap(), dir.join(TRASH_STAGING_DIR));
        assert_eq!(
            gateway_config_path().unwrap(),
            dir.join(GATEWAY_CONFIG_FILE)
        );
        assert_eq!(shell_config_path().unwrap(), dir.join(SHELL_CONFIG_FILE));
        assert_eq!(
            pricing_overrides_path().unwrap(),
            dir.join(PRICING_OVERRIDES_FILE)
        );

        // 阶段 1 不做任何读写，目录此刻可以不存在（也可能已存在），都不应影响推导结果。
        let _ = dir.exists();
    }

    /// 守住空主目录漏洞：手写实现会把空 `HOME`/`USERPROFILE` 当有效值，数据落到进程 CWD。
    /// 正常环境下恒真，价值在拦住将来改回手写实现的人。
    #[test]
    fn data_dir_is_never_a_relative_path() {
        if let Some(dir) = data_dir() {
            assert!(dir.is_absolute(), "数据目录必须是绝对路径，实际是 {dir:?}");
        }
    }
}
