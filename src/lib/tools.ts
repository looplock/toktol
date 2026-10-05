/**
 * 受支持工具的展示清单：`id` 与 Rust 侧 `toktol_core::model::Tool::ALL` 的
 * `as_str()` 值一一对应（数据库 `tool` 列的值），增删工具两侧要同步。
 * 图标来自 `@lobehub/icons-static-svg`（?raw 内联进 DOM）：彩色版带品牌色，
 * mono 版用 currentColor 随主题变色。WorkBuddy / ZCode 用官方图标（src/assets
 * 供应商资产）。
 */

export interface ToolItem {
  readonly id: string;
  readonly label: string;
  /** 无图标时的字母徽章底色。 */
  readonly badge: string;
  /** 内联 SVG 标记；缺失则回落字母徽章。 */
  readonly icon?: string;
}

import zcodeIcon from "../assets/zcode.svg?raw";
import workBuddyIcon from "../assets/workbuddy.svg?raw";
import claudeCodeIcon from "@lobehub/icons-static-svg/icons/claudecode-color.svg?raw";
import codeBuddyIcon from "@lobehub/icons-static-svg/icons/codebuddy-color.svg?raw";
import codexIcon from "@lobehub/icons-static-svg/icons/codex-color.svg?raw";
import deepSeekIcon from "@lobehub/icons-static-svg/icons/deepseek-color.svg?raw";
import grokIcon from "@lobehub/icons-static-svg/icons/grok.svg?raw";
import openCodeIcon from "@lobehub/icons-static-svg/icons/opencode.svg?raw";
import piIcon from "@lobehub/icons-static-svg/icons/pi.svg?raw";

export const TOOL_ITEMS: readonly ToolItem[] = [
  { id: "claude-code", label: "Claude Code", badge: "#D97757", icon: claudeCodeIcon },
  { id: "codebuddy", label: "CodeBuddy", badge: "#334155", icon: codeBuddyIcon },
  { id: "codex", label: "Codex", badge: "#0EA5E9", icon: codexIcon },
  { id: "dsh", label: "DSH", badge: "#8B5CF6", icon: deepSeekIcon },
  { id: "grok", label: "Grok Build", badge: "#111827", icon: grokIcon },
  { id: "opencode", label: "OpenCode", badge: "#F59E0B", icon: openCodeIcon },
  { id: "pi", label: "Pi", badge: "#64748B", icon: piIcon },
  { id: "workbuddy", label: "WorkBuddy", badge: "#10B981", icon: workBuddyIcon },
  { id: "zcode", label: "ZCode", badge: "#0D9488", icon: zcodeIcon },
];

/** 工具 id → 展示名；未收录的 id（旧库数据等）原样返回兜底。 */
export function toolLabel(id: string): string {
  return TOOL_ITEMS.find((item) => item.id === id)?.label ?? id;
}
