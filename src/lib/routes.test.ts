import { describe, expect, it } from "vitest";

import { DEFAULT_PAGE, PAGE_ORDER, type PageId, stepPage } from "./routes";

describe("PAGE_ORDER", () => {
  it("默认页面在顺序表里（否则程序一启动就是个不存在的页）", () => {
    expect(PAGE_ORDER).toContain(DEFAULT_PAGE);
  });

  it("没有重复项（重复会让 `Record<PageId, …>` 静默丢页面）", () => {
    expect(new Set(PAGE_ORDER).size).toBe(PAGE_ORDER.length);
  });
});

describe("stepPage", () => {
  const last = PAGE_ORDER[PAGE_ORDER.length - 1] as PageId;

  it("正向移动", () => {
    expect(stepPage("overview", 1)).toBe("details");
  });

  it("负向移动", () => {
    expect(stepPage("details", -1)).toBe("overview");
  });

  it("越过末尾回到开头", () => {
    expect(stepPage(last, 1)).toBe(PAGE_ORDER[0]);
  });

  it("越过开头回到末尾", () => {
    expect(stepPage("overview", -1)).toBe(last);
  });

  it("偏移量超过一圈仍然落在表内", () => {
    expect(PAGE_ORDER).toContain(stepPage("overview", PAGE_ORDER.length + 2));
    expect(PAGE_ORDER).toContain(stepPage("overview", -PAGE_ORDER.length - 2));
  });
});
