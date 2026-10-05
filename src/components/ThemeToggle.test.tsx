import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { stringsFor } from "../i18n/strings";
import { nextThemeMode, type ThemeMode } from "../lib/theme";
import { ThemeToggle } from "./ThemeToggle";

// 用 react-dom/server：这里只验"渲染出什么"，点击行为已在 theme.ts 的 nextThemeMode 里测过。

const MODES: readonly ThemeMode[] = ["light", "dark", "system"];

function label(mode: ThemeMode): string {
  const strings = stringsFor();
  const names = { light: strings.themeLight, dark: strings.themeDark, system: strings.themeSystem };

  return strings.themeToggleHint
    .replace("{current}", names[mode])
    .replace("{next}", names[nextThemeMode(mode)]);
}

it("无障碍名同时说出当前态与按下后的下一态", () => {
  for (const mode of MODES) {
    const html = renderToStaticMarkup(
      <ThemeToggle mode={mode} onChange={() => {}} label={label(mode)} />,
    );

    expect(html).toContain(`aria-label="${label(mode)}"`);
  }
});

it("三态各画一个不同的图标：只靠形状区分，不靠颜色", () => {
  const shapes = MODES.map((mode) => {
    const html = renderToStaticMarkup(
      <ThemeToggle mode={mode} onChange={() => {}} label={label(mode)} />,
    );

    // 图标是按钮内的 svg；比 path 的 d 而不是整段 html（aria-label 也会变）。
    return /<svg[^>]*>([\s\S]*?)<\/svg>/.exec(html)?.[1] ?? "";
  });

  expect(new Set(shapes).size).toBe(MODES.length);
});
