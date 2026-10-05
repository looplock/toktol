import { describe, expect, it } from "vitest";

import { LS_KEY_LAST_PAGE } from "../constants";
import { parsePageId, readStoredPage, storePage } from "./lastPage";

describe("parsePageId", () => {
  it("不认识的输入返回 null（不猜、不回落）", () => {
    expect(parsePageId("Overview")).toBeNull();
    expect(parsePageId("overview ")).toBeNull();
    expect(parsePageId("")).toBeNull();
    expect(parsePageId(null)).toBeNull();
    expect(parsePageId(undefined)).toBeNull();
  });

  it("页面表里的 id 原样通过", () => {
    expect(parsePageId("overview")).toBe("overview");
    expect(parsePageId("settings")).toBe("settings");
  });
});

describe("读取 localStorage", () => {
  it("存过就读出来（也顺带守住读的是 LAST_PAGE 这个键）", () => {
    const fake = new Map<string, string>([[LS_KEY_LAST_PAGE, "sessions"]]);
    const storage = { getItem: (key: string) => fake.get(key) ?? null };

    expect(readStoredPage(storage)).toBe("sessions");
  });

  it("存了不认识的值当作没存（旧版本或手改的脏数据不能带偏路由）", () => {
    const fake = new Map<string, string>([[LS_KEY_LAST_PAGE, "dashboard"]]);
    const storage = { getItem: (key: string) => fake.get(key) ?? null };

    expect(readStoredPage(storage)).toBeNull();
  });

  it("localStorage 抛异常时静默降级，不影响会话", () => {
    const broken = {
      getItem: () => {
        throw new Error("storage disabled");
      },
    };

    expect(readStoredPage(broken)).toBeNull();
  });
});

describe("写入 localStorage", () => {
  it("写入用的键与读取一致", () => {
    const writes: [string, string][] = [];
    const storage = { setItem: (key: string, value: string) => writes.push([key, value]) };

    storePage("gateway", storage);
    expect(writes).toEqual([[LS_KEY_LAST_PAGE, "gateway"]]);
  });

  it("写不进去静默跳过", () => {
    const broken = {
      setItem: () => {
        throw new Error("storage full");
      },
    };

    expect(() => storePage("overview", broken)).not.toThrow();
  });
});
