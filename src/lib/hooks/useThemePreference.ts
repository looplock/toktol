/**
 * 主题偏好的 React 绑定：只收敛"读偏好 → 监听系统 → 解析 → 写 DOM → 持久化"这条副作用链。
 * 判断规则一律留在 theme.ts（那里有单测），这里不新增规则。
 */

import { useEffect, useState } from "react";

import {
  applyTheme,
  readStoredThemeSettings,
  resolveMode,
  storeThemeMode,
  type ThemeMode,
} from "../theme";

function matches(query: string): boolean {
  return typeof window.matchMedia === "function" && window.matchMedia(query).matches;
}

export function useThemePreference(): [ThemeMode, (mode: ThemeMode) => void] {
  const [mode, setMode] = useState<ThemeMode>(() => readStoredThemeSettings());
  const [systemDark, setSystemDark] = useState(() =>
    matches("(prefers-color-scheme: dark)"),
  );

  useEffect(() => {
    if (mode !== "system" || typeof window.matchMedia !== "function") return;

    // 从别的模式切回"跟随系统"时，systemDark 是上次监听留下的旧值，必须重读一次。
    setSystemDark(matches("(prefers-color-scheme: dark)"));

    const query = window.matchMedia("(prefers-color-scheme: dark)");
    const onSystemChange = (event: MediaQueryListEvent) => setSystemDark(event.matches);

    query.addEventListener("change", onSystemChange);
    return () => query.removeEventListener("change", onSystemChange);
  }, [mode]);

  const resolved = resolveMode(mode, systemDark);

  useEffect(() => {
    applyTheme(resolved);
    storeThemeMode(mode);
  }, [resolved, mode]);

  return [mode, setMode];
}
