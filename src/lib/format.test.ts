import { describe, expect, it } from "vitest";

import {
  formatCostMicros,
  formatCostMicrosExact,
  formatCount,
  formatDayRange,
  formatTimeShort,
  formatTimestamp,
  formatTokens,
} from "./format";
import type { NumberUnit } from "./numberUnit";

const en: NumberUnit = "english";
const zh: NumberUnit = "chinese";
const plain: NumberUnit = "plain";

describe("formatCostMicros", () => {
  it("keeps four decimals under one dollar", () => {
    expect(formatCostMicros(10_500)).toBe("$0.0105");
    expect(formatCostMicros(500_000)).toBe("$0.5000");
  });

  it("switches to two decimals at or above one dollar", () => {
    expect(formatCostMicros(1_000_000)).toBe("$1.00");
    expect(formatCostMicros(10_500_000_000)).toBe("$10500.00");
  });

  it("renders zero as an exact zero, not unknown", () => {
    expect(formatCostMicros(0)).toBe("$0.00");
  });
});

describe("formatCount", () => {
  it("groups thousands", () => {
    expect(formatCount(1234567)).toBe("1,234,567");
  });
});

describe("formatCostMicrosExact", () => {
  it("keeps micro-dollar precision under one dollar", () => {
    expect(formatCostMicrosExact(5260)).toBe("$0.00526000");
    expect(formatCostMicrosExact(1152)).toBe("$0.00115200");
    expect(formatCostMicrosExact(208)).toBe("$0.00020800");
  });

  it("falls back to two decimals at or above one dollar and for zero", () => {
    expect(formatCostMicrosExact(1_000_000)).toBe("$1.00");
    expect(formatCostMicrosExact(0)).toBe("$0.00");
  });
});

describe("formatTokens", () => {
  it("english: keeps small numbers as-is", () => {
    expect(formatTokens(422, en)).toBe("422");
    expect(formatTokens(0, en)).toBe("0");
  });

  it("english: uses K with trailing zeros trimmed", () => {
    expect(formatTokens(91_890, en)).toBe("91.89K");
    expect(formatTokens(146_900, en)).toBe("146.9K");
    expect(formatTokens(86_010, en)).toBe("86.01K");
    expect(formatTokens(177_570, en)).toBe("177.57K");
  });

  it("english: drops trailing zeros from triple-digit K values too", () => {
    expect(formatTokens(246_800, en)).toBe("246.8K");
  });

  it("english: escalates to M/B/T past each unit", () => {
    expect(formatTokens(1_200_000, en)).toBe("1.2M");
    expect(formatTokens(53_862_200, en)).toBe("53.86M");
    expect(formatTokens(2_500_000_000, en)).toBe("2.5B");
    expect(formatTokens(1_000_000_000_000, en)).toBe("1T");
  });

  it("chinese: 逢万进位，两位小数去尾零", () => {
    expect(formatTokens(422, zh)).toBe("422");
    expect(formatTokens(91_890, zh)).toBe("9.19万");
    expect(formatTokens(146_900, zh)).toBe("14.69万");
    expect(formatTokens(155_390, zh)).toBe("15.54万");
    expect(formatTokens(260_000_000, zh)).toBe("2.6亿");
    expect(formatTokens(1_500_000_000_000, zh)).toBe("1.5万亿");
  });

  it("plain: 全量千分位，不缩写", () => {
    expect(formatTokens(422, plain)).toBe("422");
    expect(formatTokens(155_390, plain)).toBe("155,390");
    expect(formatTokens(1_200_000, plain)).toBe("1,200,000");
  });
});

describe("formatDayRange", () => {
  it("同年区间不带年份", () => {
    expect(
      formatDayRange(
        new Date(2026, 8, 1).getTime(),
        new Date(2026, 8, 26).getTime(),
      ),
    ).toBe("09-01 ~ 09-26");
  });

  it("跨年区间两侧都带年份，否则终点年份有歧义", () => {
    expect(
      formatDayRange(
        new Date(2025, 8, 1).getTime(),
        new Date(2026, 3, 4).getTime(),
      ),
    ).toBe("2025-09-01 ~ 2026-04-04");
  });

  it("12 月 31 日到 1 月 1 日也算跨年", () => {
    expect(
      formatDayRange(
        new Date(2025, 11, 31).getTime(),
        new Date(2026, 0, 1).getTime(),
      ),
    ).toBe("2025-12-31 ~ 2026-01-01");
  });
});

describe("formatTimestamp", () => {
  it("formats epoch ms as fixed-width local time", () => {
    // 用本地时区的 Date 构造输入，期望值就与时区无关，CI 三平台都能过。
    expect(formatTimestamp(new Date(2026, 8, 26, 9, 5, 3).getTime())).toBe(
      "2026-09-26 09:05:03",
    );
  });

  it("zero-pads single-digit fields", () => {
    expect(formatTimestamp(new Date(2026, 0, 1, 0, 0, 0).getTime())).toBe(
      "2026-01-01 00:00:00",
    );
  });
});

describe("formatTimeShort", () => {
  it("formats 24-hour HH:mm:ss for the given locale", () => {
    // 本地时区构造输入，期望值与时区无关；显式传界面语言，不随宿主 locale 漂移。
    const ms = new Date(2026, 8, 26, 14, 5, 3).getTime();
    expect(formatTimeShort(ms, "zh-CN")).toBe("14:05:03");
    expect(formatTimeShort(ms, "en")).toBe("14:05:03");
  });
});
