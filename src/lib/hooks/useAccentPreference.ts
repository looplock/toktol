/**
 * 强调色偏好的 React 绑定：只收敛"读偏好 → 写 DOM → 持久化"这条副作用链。
 * 与 useThemePreference 同构但不合并——明暗要跟 OS 实时走，强调色是静态偏好，
 * 合并只会让两个不相关的状态共享一次重渲染。
 */

import { useEffect, useState } from "react";

import {
  applyAccent,
  readStoredAccentSettings,
  storeAccent,
  type AccentId,
} from "../theme";

export function useAccentPreference(): [AccentId, (accent: AccentId) => void] {
  const [accent, setAccent] = useState<AccentId>(() => readStoredAccentSettings());

  useEffect(() => {
    applyAccent(accent);
    storeAccent(accent);
  }, [accent]);

  return [accent, setAccent];
}
