import { expect, it, vi } from "vitest";
import { defaultTimeSelection } from "./filters";

// 两页的初始/重置时间段都走这里：固定钟表验证"今天"的本地时区边界。
it("默认时间段是今天（本地零点起，含全天）", () => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date(2026, 8, 27, 15, 30));

  const { preset, range } = defaultTimeSelection();
  expect(preset).toBe("today");
  expect(range?.start).toBe(new Date(2026, 8, 27, 0, 0, 0, 0).getTime());
  expect(range?.end).toBe(new Date(2026, 8, 27, 23, 59, 59, 999).getTime());

  vi.useRealTimers();
});
