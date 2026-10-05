import { describe, expect, it } from "vitest";

import { LS_KEY_ACCENT, LS_KEY_THEME } from "../constants";
import {
  ACCENT_IDS,
  DEFAULT_ACCENT,
  DEFAULT_THEME_MODE,
  THEME_MODES,
  applyAccent,
  nextThemeMode,
  parseAccent,
  parseThemeMode,
  readStoredAccent,
  readStoredAccentSettings,
  readStoredThemeMode,
  readStoredThemeSettings,
  resolveMode,
  storeAccent,
  storeThemeMode,
  type ThemeMode,
} from "./theme";

describe("THEME_MODES", () => {
  it("缺省模式在表里", () => {
    expect(THEME_MODES).toContain(DEFAULT_THEME_MODE);
  });

  it("没有重复项", () => {
    expect(new Set(THEME_MODES).size).toBe(THEME_MODES.length);
  });
});

describe("parseThemeMode", () => {
  it("接受三个合法值", () => {
    expect(parseThemeMode("light")).toBe("light");
    expect(parseThemeMode("dark")).toBe("dark");
    expect(parseThemeMode("system")).toBe("system");
  });

  it("不认识的输入返回 null（不猜、不回落）", () => {
    expect(parseThemeMode("Light")).toBeNull();
    expect(parseThemeMode("auto")).toBeNull();
    expect(parseThemeMode("default-dark")).toBeNull();
    expect(parseThemeMode("")).toBeNull();
    expect(parseThemeMode(null)).toBeNull();
    expect(parseThemeMode(undefined)).toBeNull();
  });
});

describe("resolveMode", () => {
  it("跟随系统时由系统偏好决定", () => {
    expect(resolveMode("system", true)).toBe("dark");
    expect(resolveMode("system", false)).toBe("light");
  });

  it("显式选择优先于系统偏好", () => {
    expect(resolveMode("light", true)).toBe("light");
    expect(resolveMode("dark", false)).toBe("dark");
  });
});

describe("nextThemeMode", () => {
  it("三态循环：亮 → 暗 → 跟随系统 → 亮", () => {
    expect(nextThemeMode("light")).toBe("dark");
    expect(nextThemeMode("dark")).toBe("system");
    expect(nextThemeMode("system")).toBe("light");
  });

  it("按三下回到起点（循环长度等于态数，不存在走不到的态）", () => {
    for (const start of THEME_MODES) {
      let mode: ThemeMode = start;

      for (let step = 0; step < THEME_MODES.length; step += 1) mode = nextThemeMode(mode);
      expect(mode).toBe(start);
    }
  });
});

describe("ACCENT_IDS", () => {
  it("缺省强调色在表里", () => {
    expect(ACCENT_IDS).toContain(DEFAULT_ACCENT);
  });

  it("没有重复项", () => {
    expect(new Set(ACCENT_IDS).size).toBe(ACCENT_IDS.length);
  });
});

describe("parseAccent", () => {
  it("接受全部合法值", () => {
    for (const accent of ACCENT_IDS) {
      expect(parseAccent(accent)).toBe(accent);
    }
  });

  it("不认识的输入返回 null（不猜、不回落）", () => {
    expect(parseAccent("Indigo")).toBeNull();
    expect(parseAccent("dracula")).toBeNull();
    expect(parseAccent("")).toBeNull();
    expect(parseAccent(null)).toBeNull();
    expect(parseAccent(undefined)).toBeNull();
  });
});

describe("applyAccent", () => {
  function fakeRoot() {
    return { dataset: {} as DOMStringMap } as HTMLElement;
  }

  it("非默认色写入 data-accent", () => {
    const root = fakeRoot();

    applyAccent("cyan", root);

    expect(root.dataset["accent"]).toBe("cyan");
  });

  it("默认色删除属性：恢复默认与从未设置是同一个 DOM 状态", () => {
    const root = fakeRoot();
    root.dataset["accent"] = "cyan";

    applyAccent("indigo", root);

    expect("accent" in root.dataset).toBe(false);
  });
});

describe("读写 localStorage", () => {
  function fakeStorage(seed: Record<string, string> = {}) {
    const map = new Map(Object.entries(seed));

    return {
      map,
      getItem: (key: string) => map.get(key) ?? null,
      setItem: (key: string, value: string) => void map.set(key, value),
    };
  }

  it("写进去再读出来", () => {
    const storage = fakeStorage();

    storeThemeMode("dark", storage);

    expect(storage.map.get(LS_KEY_THEME)).toBe("dark");
    expect(readStoredThemeMode(storage)).toBe("dark");
  });

  it("没存过时跟随系统", () => {
    expect(readStoredThemeMode(fakeStorage())).toBeNull();
    expect(readStoredThemeSettings(fakeStorage())).toBe("system");
  });

  it("存了不认识的值时跟随系统，而不是猜一个", () => {
    expect(readStoredThemeSettings(fakeStorage({ [LS_KEY_THEME]: "dracula" }))).toBe(
      "system",
    );
  });

  it("localStorage 抛异常时静默降级，不影响会话", () => {
    const broken = {
      getItem: () => {
        throw new Error("storage disabled");
      },
      setItem: () => {
        throw new Error("storage disabled");
      },
    };

    expect(() => storeThemeMode("light", broken)).not.toThrow();
    expect(readStoredThemeMode(broken)).toBeNull();
    expect(readStoredThemeSettings(broken)).toBe("system");
  });
});

describe("读写 localStorage（强调色）", () => {
  function fakeStorage(seed: Record<string, string> = {}) {
    const map = new Map(Object.entries(seed));

    return {
      map,
      getItem: (key: string) => map.get(key) ?? null,
      setItem: (key: string, value: string) => void map.set(key, value),
    };
  }

  it("写进去再读出来", () => {
    const storage = fakeStorage();

    storeAccent("purple", storage);

    expect(storage.map.get(LS_KEY_ACCENT)).toBe("purple");
    expect(readStoredAccent(storage)).toBe("purple");
  });

  it("没存过时是默认强调色", () => {
    expect(readStoredAccent(fakeStorage())).toBeNull();
    expect(readStoredAccentSettings(fakeStorage())).toBe(DEFAULT_ACCENT);
  });

  it("存了不认识的值时回到默认强调色，而不是猜一个", () => {
    expect(readStoredAccentSettings(fakeStorage({ [LS_KEY_ACCENT]: "dracula" }))).toBe(
      DEFAULT_ACCENT,
    );
  });

  it("localStorage 抛异常时静默降级，不影响会话", () => {
    const broken = {
      getItem: () => {
        throw new Error("storage disabled");
      },
      setItem: () => {
        throw new Error("storage disabled");
      },
    };

    expect(() => storeAccent("amber", broken)).not.toThrow();
    expect(readStoredAccent(broken)).toBeNull();
    expect(readStoredAccentSettings(broken)).toBe(DEFAULT_ACCENT);
  });
});
