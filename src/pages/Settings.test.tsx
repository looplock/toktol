import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { stringsFor } from "../i18n/strings";
import { ACCENT_IDS, type AccentId } from "../lib/theme";
import { TOOL_ITEMS } from "../lib/tools";
import { SettingsPage } from "./Settings";

// 用 react-dom/server 而不是 jsdom：这里只验"渲染出什么"，不碰交互，
// 没必要为它引一个 DOM 环境。全部分区常驻、非激活的用 hidden，静态标记可见。

const noop = () => {};
const strings = stringsFor();

/** accent id → 文案（与 Settings.tsx 的 ACCENT_LABEL_KEYS 同一映射，漂移由 UI 呈现兜底）。 */
const ACCENT_LABELS: Record<AccentId, string> = {
  indigo: strings.accentIndigo,
  blue: strings.accentBlue,
  cyan: strings.accentCyan,
  slate: strings.accentSlate,
  purple: strings.accentPurple,
  fuchsia: strings.accentFuchsia,
  pink: strings.accentPink,
  amber: strings.accentAmber,
  yellow: strings.accentYellow,
  green: strings.accentGreen,
};

function renderSettings(overrides: Partial<Parameters<typeof SettingsPage>[0]> = {}) {
  return renderToStaticMarkup(
    <SettingsPage
      strings={strings}
      locale="zh-CN"
      onLocaleChange={noop}
      mode="system"
      onModeChange={noop}
      accent="indigo"
      onAccentChange={noop}
      disabledTools={[]}
      onDisabledToolsChange={noop}
      inputScope="input"
      onInputScopeChange={noop}
      numberUnit="english"
      onNumberUnitChange={noop}
      chartAnimation="on"
      onChartAnimationChange={noop}
      trayEnabled
      onTrayEnabledChange={noop}
      autostart={false}
      onAutostartChange={noop}
      {...overrides}
    />,
  );
}

it("外观有明暗与强调色两行：选项互斥，不含下拉框", () => {
  const html = renderSettings();

  expect(html).not.toContain("<select");
  expect(html).toContain(strings.themeLight);
  expect(html).toContain(strings.themeDark);
  expect(html).toContain(strings.themeSystem);
  // 分段控件用原生 radio：整组只有一个 Tab 停靠点，方向键可移动。
  expect(html).toContain('type="radio"');
  // 强调色是一排色板按钮，每个预设一枚，用 aria-pressed 表达选中。
  expect(html).toContain(strings.settingsAccent);
  expect(html.split('type="button"').length - 1).toBeGreaterThanOrEqual(ACCENT_IDS.length);
  for (const accent of ACCENT_IDS) {
    expect(html).toContain(`aria-label="${ACCENT_LABELS[accent]}"`);
  }
});

it("当前选中的强调色只有一枚呈按下态", () => {
  const html = renderSettings({ accent: "cyan" });

  expect(html.split('aria-pressed="true"').length - 1).toBe(1);
});

it("四个分区都在导航里，未激活的内容面板以 hidden 收起", () => {
  const html = renderSettings();

  expect(html).toContain(strings.settingsAppearance);
  expect(html).toContain(strings.settingsDisplay);
  expect(html).toContain(strings.settingsBackground);
  expect(html).toContain(strings.settingsTools);
  // 外观激活，另外三个面板收起。
  expect(html.split(" hidden").length - 1).toBe(3);
});

it("工具启用区列出全部受支持工具，每个一把开关", () => {
  const html = renderSettings();

  for (const tool of TOOL_ITEMS) {
    expect(html).toContain(tool.label);
  }
  expect(html).toContain(strings.settingsToolsHint);
  // 开关用原生 checkbox + role="switch"；"后台与托盘"区还有托盘/自启动两把固定开关。
  expect(html.split('role="switch"').length - 1).toBe(TOOL_ITEMS.length + 2);
});

it("被禁用的工具开关呈未勾选态", () => {
  const html = renderSettings({ disabledTools: ["pi"] });

  const switches = [...html.matchAll(/role="switch"[^>]*/g)].map((m) => m[0]);
  expect(switches.length).toBe(TOOL_ITEMS.length + 2);
  // 只有 Pi 一把关着（托盘开着、自启动关着，合计固定两把一开一关）。
  expect(switches.filter((tag) => tag.includes("checked")).length).toBe(TOOL_ITEMS.length - 1 + 1);
});

it("后台与托盘区有系统托盘与开机自启动两行", () => {
  const html = renderSettings();

  expect(html).toContain(strings.settingsTray);
  expect(html).toContain(strings.settingsTrayHint);
  expect(html).toContain(strings.settingsAutostart);
  expect(html).toContain(strings.settingsAutostartHint);
  // 关掉托盘：它的开关呈未勾选态，自启动仍勾在缺省的关上。
  const htmlTrayOff = renderSettings({ trayEnabled: false });
  const trayRow = htmlTrayOff.slice(htmlTrayOff.indexOf(strings.settingsTray));
  const traySwitch = trayRow.slice(trayRow.indexOf('role="switch"'));
  expect(traySwitch.slice(0, 60)).not.toContain("checked");
});

it("显示区有输入口径两档，选中态随偏好走", () => {
  const html = renderSettings({ inputScope: "inputWithCacheRead" });

  expect(html).toContain(strings.settingsInputScope);
  expect(html).toContain(strings.settingsInputScopePlain);
  expect(html).toContain(strings.settingsInputScopeWithCache);
  // 分段控件用原生 radio，两档各一枚，勾选在"含缓存读"上。
  const radios = [...html.matchAll(/type="radio"[^>]*/g)].map((m) => m[0]);
  expect(radios.some((tag) => tag.includes('value="inputWithCacheRead"') && tag.includes("checked"))).toBe(
    true,
  );
  expect(radios.some((tag) => tag.includes('value="input"') && tag.includes("checked"))).toBe(false);
});

it("显示区有单位制三档，选中态随偏好走", () => {
  const html = renderSettings({ numberUnit: "chinese" });

  expect(html).toContain(strings.settingsNumberUnit);
  expect(html).toContain(strings.settingsNumberUnitChinese);
  expect(html).toContain(strings.settingsNumberUnitEnglish);
  expect(html).toContain(strings.settingsNumberUnitPlain);
  const radios = [...html.matchAll(/type="radio"[^>]*/g)].map((m) => m[0]);
  expect(radios.some((tag) => tag.includes('value="chinese"') && tag.includes("checked"))).toBe(true);
});

it("显示区有语言两档，选项用各自的自称", () => {
  const html = renderSettings();

  expect(html).toContain(strings.settingsLanguage);
  expect(html).toContain(">中文</span>");
  expect(html).toContain(">English</span>");
});
