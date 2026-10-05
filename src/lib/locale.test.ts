import { describe, expect, it } from "vitest";

import { LS_KEY_LOCALE } from "../constants";
import { parseLocale, readStoredLocale } from "./locale";

describe("parseLocale", () => {
  it("不认识的输入返回 null（不猜、不回落）", () => {
    // 大小写敏感、不接受前缀、不接受空值。
    expect(parseLocale("ZH-CN")).toBeNull();
    expect(parseLocale("zh")).toBeNull();
    expect(parseLocale("zh-CN-x")).toBeNull();
    expect(parseLocale("")).toBeNull();
    expect(parseLocale(null)).toBeNull();
    expect(parseLocale(undefined)).toBeNull();
  });
});

describe("读取 localStorage", () => {
  it("存过就读出来（也顺带守住读的是 LOCALE 这个键）", () => {
    const fake = new Map<string, string>([[LS_KEY_LOCALE, "zh-CN"]]);
    const storage = { getItem: (key: string) => fake.get(key) ?? null };

    expect(readStoredLocale(storage)).toBe("zh-CN");
  });

  it("存了不认识的值当作没存（不能原样返回未校验的字符串）", () => {
    const fake = new Map<string, string>([[LS_KEY_LOCALE, "klingon"]]);
    const storage = { getItem: (key: string) => fake.get(key) ?? null };

    expect(readStoredLocale(storage)).toBeNull();
  });

  it("localStorage 抛异常时静默降级，不影响会话", () => {
    const broken = {
      getItem: () => {
        throw new Error("storage disabled");
      },
    };

    expect(readStoredLocale(broken)).toBeNull();
  });
});
