import { afterEach, expect, it } from "vitest";

import { LS_KEY_TOOLS_DISABLED } from "../constants";
import { readDisabledTools, writeDisabledTools } from "./toolPrefs";

// node 环境没有 localStorage：用 Map 桩替代，只验存取逻辑与坏数据的容错。
const store = new Map<string, string>();

globalThis.localStorage = {
  getItem: (key: string) => store.get(key) ?? null,
  setItem: (key: string, value: string) => void store.set(key, value),
} as Storage;

afterEach(() => store.clear());

it("reads an empty list when nothing is stored", () => {
  expect(readDisabledTools()).toEqual([]);
});

it("round-trips the disabled list", () => {
  writeDisabledTools(["pi", "grok"]);
  expect(readDisabledTools()).toEqual(["pi", "grok"]);
  expect(store.get(LS_KEY_TOOLS_DISABLED)).toBe('["pi","grok"]');
});

it("falls back to all-enabled on corrupt data", () => {
  store.set(LS_KEY_TOOLS_DISABLED, "{ not json");
  expect(readDisabledTools()).toEqual([]);
  store.set(LS_KEY_TOOLS_DISABLED, JSON.stringify([1, "pi", null]));
  // 非字符串条目丢弃。
  expect(readDisabledTools()).toEqual(["pi"]);
});
