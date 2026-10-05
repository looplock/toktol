/**
 * 开关。语义与键盘（空格切换）交给原生 checkbox + role="switch"；
 * 轨道与滑块的状态要读 checked，而 peer 变体够不到内层元素，所以外观用受控 class 画。
 */

interface SwitchProps {
  readonly label: string;
  readonly checked: boolean;
  readonly onChange: (checked: boolean) => void;
  /** 操作进行中或服务不可用时禁用；外观同步弱化，不只挡点击。 */
  readonly disabled?: boolean;
  /** 行内已有说明（如工具名）时藏起文字，只留给读屏。 */
  readonly labelHidden?: boolean;
}

export function Switch({ label, checked, onChange, disabled = false, labelHidden = false }: SwitchProps) {
  return (
    <label
      className={`flex items-center gap-3 ${disabled ? "opacity-50" : "cursor-pointer"}`}
    >
      <input
        type="checkbox"
        role="switch"
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
        className="peer sr-only"
      />
      <span
        aria-hidden="true"
        className={`flex h-5 w-9 shrink-0 items-center rounded-full border px-px transition-colors peer-focus-visible:outline-2 peer-focus-visible:outline-offset-2 peer-focus-visible:outline-ring ${
          checked ? "border-accent bg-accent" : "border-border bg-surface-muted"
        }`}
      >
        {/* 垂直居中交给 flex，不依赖 top 偏移：分数缩放下像素偏移会渲染发虚。 */}
        <span
          className={`size-4 rounded-full transition-transform ${
            checked ? "translate-x-4 bg-accent-ink" : "bg-ink-muted"
          }`}
        />
      </span>
      <span className={labelHidden ? "sr-only" : "text-sm"}>{label}</span>
    </label>
  );
}
