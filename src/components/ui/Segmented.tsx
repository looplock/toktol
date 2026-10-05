/**
 * 分段控件：一组互斥选项。语义与键盘行为交给原生 radio（同名 radio 之间方向键可移动、
 * 整组只有一个 Tab 停靠点），受控 class 只负责画外观。
 * label 支持 ReactNode：纯图标选项请自带 sr-only 文本，保证 radio 可朗读。
 * size：md 为设置页等宽松表单的默认档；sm 供卡片头部等密集场景。
 */

import { useId, type ReactNode } from "react";

export interface SegmentedOption<T extends string> {
  readonly value: T;
  readonly label: ReactNode;
}

type SegmentedSize = "sm" | "md";

const SIZE: Record<SegmentedSize, { fieldset: string; option: string }> = {
  sm: {
    fieldset: "gap-0.5 p-1",
    option: "px-2 py-0.5 text-xs",
  },
  md: {
    fieldset: "gap-0.5 p-1",
    option: "px-1.5 py-0.5 text-sm",
  },
};

interface SegmentedProps<T extends string> {
  readonly label: string;
  readonly options: readonly SegmentedOption<T>[];
  readonly value: T;
  readonly onChange: (value: T) => void;
  readonly disabled?: boolean;
  readonly size?: SegmentedSize;
}

export function Segmented<T extends string>({
  label,
  options,
  value,
  onChange,
  disabled = false,
  size = "md",
}: SegmentedProps<T>) {
  const name = useId();

  return (
    <fieldset
      disabled={disabled}
      className={`flex w-fit flex-wrap items-center rounded-control bg-surface-subtle disabled:opacity-50 ${SIZE[size].fieldset}`}
    >
      <legend className="sr-only">{label}</legend>

      {options.map((option) => {
        const selected = option.value === value;

        return (
          <label key={option.value} className={disabled ? "" : "cursor-pointer"}>
            <input
              type="radio"
              name={name}
              value={option.value}
              checked={selected}
              onChange={() => onChange(option.value)}
              className="peer sr-only"
            />
            {/* 真正的 input 是 sr-only，焦点环必须画在这里，否则键盘焦点等于不可见。 */}
            <span
              className={`block rounded-control peer-focus-visible:outline-2 peer-focus-visible:outline-offset-2 peer-focus-visible:outline-ring ${SIZE[size].option} ${
                selected
                  ? "bg-accent font-medium text-accent-ink"
                  : "text-ink-muted hover:text-ink"
              }`}
            >
              {option.label}
            </span>
          </label>
        );
      })}
    </fieldset>
  );
}
