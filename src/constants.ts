/**
 * 前端侧常量，与 Rust 侧 `toktol_core::paths` 一一镜像；跨语言同步由
 * scripts/verify-constants-parity.mjs 强制（两侧各自的单测发现不了漂移）。
 * 刻意不写"常量等于字面量"的单测——那是把同一句话写两遍的虚假安全感。
 */

export const PRODUCT_NAME = "Toktol";

export const APP_IDENTIFIER = "app.toktol.desktop";

export const DATA_DIR_NAME = ".toktol";

export const GATEWAY_TOKEN_PREFIX = "sk-toktol-";

export const LS_KEY_LOCALE = "toktol-locale";

export const LS_KEY_THEME = "toktol-theme";

export const LS_KEY_ACCENT = "toktol-accent";

export const LS_KEY_TOOLS_DISABLED = "toktol-tools-disabled";

export const LS_KEY_INPUT_SCOPE = "toktol-input-scope";

export const LS_KEY_NUMBER_UNIT = "toktol-number-unit";

export const LS_KEY_CHART_ANIMATION = "toktol-chart-animation";

export const LS_KEY_LAST_PAGE = "toktol-last-page";

