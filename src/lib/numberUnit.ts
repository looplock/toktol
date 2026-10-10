/**
 * token 数量的单位制偏好与 React 绑定：中文（万/亿）/ 英文（K/M）/ 无单位。
 * 只作用于 token 数量；调用次数、请求数等普通计数不受影响。
 * 纯前端显示偏好（归属同明暗/强调色），Rust 侧只在 paths.rs 存键名做镜像。
 */

import { createContext, useContext } from "react";

import { LS_KEY_NUMBER_UNIT } from "../constants";

export const NUMBER_UNITS = ["chinese", "english", "plain"] as const;

/** 单位制：`chinese` 万/亿进位，`english` K/M/B/T 进位，`plain` 千分位不缩写。 */
export type NumberUnit = (typeof NUMBER_UNITS)[number];

/** 中文单位制的进位后缀（大→小）。这是 `chinese` 偏好的语义本体，不是界面
 * 文案：用户选了这套单位就永远显示"万亿/亿/万"（同 K/M/B/T 之于 `english`），
 * 不随界面语言切换——i18n 门禁按此理由豁免本文件。 */
export const CHINESE_TOKEN_SUFFIXES = {
  trillion: "万亿",
  hundredMillion: "亿",
  tenThousand: "万",
} as const;

export const DEFAULT_NUMBER_UNIT: NumberUnit = "english";

function parseNumberUnit(value: string | null): NumberUnit | null {
  return (NUMBER_UNITS as readonly string[]).includes(value ?? "")
    ? (value as NumberUnit)
    : null;
}

function readStored(
  key: string,
  storage: Pick<Storage, "getItem">,
): string | null {
  try {
    return storage.getItem(key);
  } catch {
    // 静默跳过：读不到就当没存过。
    return null;
  }
}

function writeStored(
  key: string,
  value: string,
  storage: Pick<Storage, "setItem">,
): void {
  try {
    storage.setItem(key, value);
  } catch {
    // 静默跳过：写不进去也不该影响本次会话。
  }
}

export function readStoredNumberUnit(
  storage: Pick<Storage, "getItem"> = localStorage,
): NumberUnit | null {
  return parseNumberUnit(readStored(LS_KEY_NUMBER_UNIT, storage));
}

export function storeNumberUnit(
  unit: NumberUnit,
  storage: Pick<Storage, "setItem"> = localStorage,
): void {
  writeStored(LS_KEY_NUMBER_UNIT, unit, storage);
}

/** 读出用户存的单位制；没存过或存了不认识的值就是英文 K/M。 */
export function readNumberUnitSettings(
  storage: Pick<Storage, "getItem"> = localStorage,
): NumberUnit {
  return readStoredNumberUnit(storage) ?? DEFAULT_NUMBER_UNIT;
}

export const NumberUnitContext = createContext<NumberUnit>(
  DEFAULT_NUMBER_UNIT,
);

/** 取当前单位制；卡片/页面组件直接调用，设置页走显式 props。 */
export function useNumberUnit(): NumberUnit {
  return useContext(NumberUnitContext);
}
