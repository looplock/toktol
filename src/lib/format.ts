/**
 * 展示格式化：微美元成本与数字。只管格式，不做任何换算或"未知"判定——
 * 那是调用方的语义责任。
 */

import { CHINESE_TOKEN_SUFFIXES, type NumberUnit } from "./numberUnit";

/** 微美元 → "$X.XX"；不足 1 美元给 4 位小数，小额不至于显示成 $0.00。 */
export function formatCostMicros(micros: number): string {
  const dollars = micros / 1_000_000;
  const digits = dollars !== 0 && Math.abs(dollars) < 1 ? 4 : 2;
  return `$${dollars.toFixed(digits)}`;
}

/** 明细表用的全精度成本：不足 1 美元固定 8 位小数（$0.00778633 这种），不去尾零。 */
export function formatCostMicrosExact(micros: number): string {
  const dollars = micros / 1_000_000;

  if (dollars !== 0 && Math.abs(dollars) < 1) {
    return `$${dollars.toFixed(8)}`;
  }

  return `$${dollars.toFixed(2)}`;
}

/** 两位小数去尾零（177.57、53.86 这种），进位单位的数值部分共用。 */
function trim2(value: number): string {
  return value.toFixed(2).replace(/0+$/, "").replace(/\.$/, "");
}

/** token 数量：按单位制进位。中文（万/亿/万亿）、英文（K/M/B/T）、无单位（千分位）。
 * 英文档千以下原样（保持既有紧凑观感），其余档千位分隔。 */
export function formatTokens(value: number, unit: NumberUnit): string {
  if (unit === "plain") {
    return new Intl.NumberFormat("en-US").format(value);
  }

  const units =
    unit === "chinese"
      ? ([
          { limit: 1e12, suffix: CHINESE_TOKEN_SUFFIXES.trillion },
          { limit: 1e8, suffix: CHINESE_TOKEN_SUFFIXES.hundredMillion },
          { limit: 1e4, suffix: CHINESE_TOKEN_SUFFIXES.tenThousand },
        ] as const)
      : ([
          { limit: 1e12, suffix: "T" },
          { limit: 1e9, suffix: "B" },
          { limit: 1e6, suffix: "M" },
          { limit: 1e3, suffix: "K" },
        ] as const);

  for (const { limit, suffix } of units) {
    if (Math.abs(value) >= limit) {
      return `${trim2(value / limit)}${suffix}`;
    }
  }

  return unit === "english" ? String(value) : new Intl.NumberFormat("en-US").format(value);
}

/** 字节 → 人类可读（B / KB / MB / GB，1024 进位，附件卡体积显示用）。 */
export function formatByteSize(bytes: number): string {
  const units = [
    { limit: 1024 ** 3, suffix: "GB" },
    { limit: 1024 ** 2, suffix: "MB" },
    { limit: 1024, suffix: "KB" },
  ] as const;

  for (const { limit, suffix } of units) {
    if (bytes >= limit) {
      const text = (bytes / limit).toFixed(1).replace(/\.0$/, "");
      return `${text} ${suffix}`;
    }
  }

  return `${bytes} B`;
}

/** epoch ms → "MM-DD"：时间区间按钮上的短日期，不带年份省宽度。 */
export function formatMonthDay(ms: number): string {
  const date = new Date(ms);
  const pad = (value: number): string => String(value).padStart(2, "0");

  return `${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

/** 时间区间按钮文案：同年 "MM-DD ~ MM-DD"，跨年两侧都带年份——"09-01 ~ 04-04" 的 04-04 看不出是哪一年。 */
export function formatDayRange(startMs: number, endMs: number): string {
  const sameYear =
    new Date(startMs).getFullYear() === new Date(endMs).getFullYear();
  const left = sameYear
    ? formatMonthDay(startMs)
    : `${new Date(startMs).getFullYear()}-${formatMonthDay(startMs)}`;
  const right = sameYear
    ? formatMonthDay(endMs)
    : `${new Date(endMs).getFullYear()}-${formatMonthDay(endMs)}`;

  return `${left} ~ ${right}`;
}

/** 千分位分组，固定 en-US：测试与截图不随宿主系统 locale 漂移。 */
export function formatCount(value: number): string {
  return new Intl.NumberFormat("en-US").format(value);
}

/** 时长单位文案由调用方传入（format.ts 不做翻译），数值部分统一 1 位小数去尾零。 */
export interface DurationUnits {
  readonly day: string;
  readonly hour: string;
  readonly minute: string;
}

/** 毫秒 → 人类可读时长：单位紧跟数字不空格（"2d 20h"），0 给 "0"、不足 1 分钟给
 * "<1min"，1 小时内取整分钟，1 天内小时保留 1 位小数，再长折成 "Xd Yh"。统计总览的使用时长用。 */
export function formatDuration(ms: number, units: DurationUnits): string {
  if (ms === 0) return "0";

  const minutes = Math.floor(ms / 60_000);

  if (minutes < 1) return `<1${units.minute}`;
  if (minutes < 60) return `${minutes}${units.minute}`;

  const hours = ms / 3_600_000;
  if (hours < 24) {
    return `${hours.toFixed(1).replace(/\.0$/, "")}${units.hour}`;
  }

  const days = Math.floor(hours / 24);
  const rest = Math.round(hours - days * 24);

  return rest === 0 ? `${days}${units.day}` : `${days}${units.day} ${rest}${units.hour}`;
}

/** epoch ms → 本地时区 "YYYY-MM-DD HH:mm:ss"。定宽 19 字符，表格列不会跳动。 */
export function formatTimestamp(ms: number): string {
  const date = new Date(ms);
  const pad = (value: number) => String(value).padStart(2, "0");

  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ` +
    `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
  );
}

/** epoch ms → 24 小时制短时间（"14:23:05"）：按传入的界面语言格式化，
 * 不落回宿主 locale（格式化结果随界面语言切换，不随系统设置漂移）。 */
export function formatTimeShort(ms: number, locale: string): string {
  return new Intl.DateTimeFormat(locale, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  }).format(ms);
}
