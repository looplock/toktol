/**
 * 应用根组件，只做编排：持有主题偏好、语言、当前页面、跨页会话焦点。
 * 加新页面不用改这里，改 routes.ts 和 pages/index.tsx 即可。
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  autostartIsEnabled,
  autostartSet,
  isTauriRuntime,
  shellGetConfig,
  shellSetDisabledTools,
  traySetEnabled,
  traySetTexts,
} from "./lib/api";
import { ScanButton } from "./components/ScanButton";
import { ScanToast } from "./components/ScanToast";
import { ThemeToggle } from "./components/ThemeToggle";
import { TopNav, type NavItem } from "./components/TopNav";
import { stringsFor } from "./i18n/strings";
import {
  readInputScopeSettings,
  storeInputScope,
  type InputScope,
} from "./lib/inputScope";
import {
  NumberUnitContext,
  readNumberUnitSettings,
  storeNumberUnit,
  type NumberUnit,
} from "./lib/numberUnit";
import {
  ChartAnimationContext,
  readChartAnimationSettings,
  storeChartAnimation,
  type ChartAnimation,
} from "./lib/chartAnimation";
import {
  DEFAULT_LOCALE,
  LocaleContext,
  readStoredLocale,
  storeLocale,
  type Locale,
} from "./lib/locale";
import { DEFAULT_PAGE, PAGE_ORDER, type PageId, type SessionFocus } from "./lib/routes";
import { readStoredPage, storePage } from "./lib/lastPage";
import { scanToastBacklogDetail, scanToastDetail, type ScanToastKind } from "./lib/scanToast";
import { readDisabledTools, writeDisabledTools } from "./lib/toolPrefs";
import { nextThemeMode, type ThemeMode } from "./lib/theme";
import { useAccentPreference } from "./lib/hooks/useAccentPreference";
import { useScanScheduler } from "./lib/hooks/useScanScheduler";
import { useScanToast } from "./lib/hooks/useScanToast";
import { useThemePreference } from "./lib/hooks/useThemePreference";
import { PAGE_LABEL_KEYS, PageView } from "./pages";

function initialLocale(): Locale {
  return readStoredLocale() ?? DEFAULT_LOCALE;
}

/** 模式名 → 文案。跟随系统也是一态，按钮上要能说出来。 */
function modeName(mode: ThemeMode, strings: ReturnType<typeof stringsFor>): string {
  if (mode === "light") return strings.themeLight;
  if (mode === "dark") return strings.themeDark;

  return strings.themeSystem;
}

/** 气泡的标题按形态取：扫描中 / 扫描完成 / 扫描失败。 */
function toastTitle(kind: ScanToastKind, strings: ReturnType<typeof stringsFor>): string {
  if (kind === "running") return strings.overviewScanning;

  return kind === "done" ? strings.overviewScanDone : strings.overviewScanFailed;
}

export default function App() {
  const [mode, setMode] = useThemePreference();
  const [accent, setAccent] = useAccentPreference();
  const [locale, setLocale] = useState<Locale>(initialLocale);
  const onLocaleChange = (next: Locale) => {
    setLocale(next);
    storeLocale(next);
  };
  // 上次停留页面跨刷新恢复（托盘低功耗模式会销毁/重建窗口，同理）；跨页
  // 会话焦点是一次性的，不跟随恢复。
  const [page, setPage] = useState<PageId>(() => readStoredPage() ?? DEFAULT_PAGE);
  useEffect(() => storePage(page), [page]);
  // 跨页会话焦点：明细页点会话 → 带着目标跳到会话页选中。手动导航即作废。
  const [sessionFocus, setSessionFocus] = useState<SessionFocus | null>(null);
  const openSession = useCallback((focus: SessionFocus) => {
    setSessionFocus(focus);
    setPage("sessions");
  }, []);
  const navigate = useCallback((next: PageId) => {
    setSessionFocus(null);
    setPage(next);
  }, []);
  const consumeFocus = useCallback(() => setSessionFocus(null), []);
  // 工具启用偏好：扫描与查询的过滤清单随 IPC 传下去，变更即持久化；同时
  // 镜像进壳配置——常驻扫描循环每轮从壳配置读清单，窗口销毁后前端传不了参。
  const [disabledTools, setDisabledTools] = useState<string[]>(readDisabledTools);

  const onDisabledToolsChange = (next: string[]) => {
    setDisabledTools(next);
    writeDisabledTools(next);
    if (isTauriRuntime()) {
      shellSetDisabledTools(next).catch(() => {});
    }
  };

  // 明细页输入口径：纯显示偏好，变更即持久化（同工具启用偏好的模式）。
  const [inputScope, setInputScope] = useState<InputScope>(readInputScopeSettings);
  const onInputScopeChange = (scope: InputScope) => {
    setInputScope(scope);
    storeInputScope(scope);
  };

  // token 数量单位制：同上的显示偏好；经 context 下发，卡片/页面各自取用。
  const [numberUnit, setNumberUnit] = useState<NumberUnit>(readNumberUnitSettings);
  const onNumberUnitChange = (unit: NumberUnit) => {
    setNumberUnit(unit);
    storeNumberUnit(unit);
  };

  // 图表生长动画：同上的显示偏好；经 context 下发，EChart 层统一消费。
  const [chartAnimation, setChartAnimation] = useState<ChartAnimation>(
    readChartAnimationSettings,
  );
  const onChartAnimationChange = (animation: ChartAnimation) => {
    setChartAnimation(animation);
    storeChartAnimation(animation);
  };

  // 自动扫描循环常驻 Rust 壳（与窗口生死无关）：dataVersion 是"库里有新
  // 数据"的信号，数据页盯着它刷新；扫描中状态给顶栏按钮。
  const scan = useScanScheduler();

  // ---- 后台与托盘 ----
  // 托盘/低功耗的事实源在 Rust 壳（config.json，启动早期就要用）：启动拉一次
  // 初值；托盘动作在壳内闭环（扫描直连循环、低功耗即销毁/重建窗口），前端无感知。
  const [trayEnabled, setTrayEnabled] = useState(true);
  const [autostart, setAutostart] = useState(false);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    shellGetConfig()
      .then((config) => setTrayEnabled(config.tray))
      .catch(() => {});
    autostartIsEnabled()
      .then(setAutostart)
      .catch(() => {});
  }, []);

  const onTrayEnabledChange = (enabled: boolean) => {
    setTrayEnabled(enabled);
    if (isTauriRuntime()) {
      traySetEnabled(enabled).catch(() => setTrayEnabled(!enabled));
    }
  };

  // 开机自启动事实源在系统（注册表/LaunchAgent）：开关只反映它，失败即回滚。
  const onAutostartChange = (enabled: boolean) => {
    setAutostart(enabled);
    if (isTauriRuntime()) {
      autostartSet(enabled).catch(() => setAutostart(!enabled));
    }
  };
  // 扫描反馈气泡：不打扰式——手动必给（含"扫描中"），后台轮次只在扫到东西时出声。
  const toast = useScanToast({
    scanning: scan.scanning,
    manual: scan.manualRun,
    outcome: scan.outcome,
    backlog: scan.backlog,
  });

  const strings = stringsFor(locale);

  // 托盘菜单文案跟随界面语言（菜单的中文缺省由壳启动时设置）。
  useEffect(() => {
    if (!isTauriRuntime()) return;
    traySetTexts(
      strings.trayMenuShow,
      strings.trayMenuScan,
      strings.trayMenuLowPower,
      strings.trayMenuQuit,
    ).catch(() => {});
  }, [strings]);

  const navItems = useMemo<NavItem[]>(
    () => PAGE_ORDER.map((id) => ({ id, label: strings[PAGE_LABEL_KEYS[id]] })),
    [strings],
  );

  const next = nextThemeMode(mode);
  const toggleLabel = strings.themeToggleHint
    .replace("{current}", modeName(mode, strings))
    .replace("{next}", modeName(next, strings));

  return (
    /* 整页不让文档滚动，只有 main 自己滚：顶栏是应用的骨架，不该跟着内容跑。
     * 用 clip 而非 hidden 是关键：hidden 的盒子仍是滚动容器，滚轮在 main 顶/底继续滚会
     * 链式传导上来，把整个壳（连顶栏）滚出视口；clip 根本不是滚动容器，链在这里断掉。 */
    <div className="flex h-full flex-col overflow-clip bg-canvas text-ink">
      <div className="shrink-0 border-b border-border bg-canvas">
        {/* 三列网格而不是 flex + justify-center：flex 只能把 tab 放在"剩下的空间"里居中，
         * 左右两侧宽度不等时看着就不在窗口正中。1fr 两侧等宽，中间 auto 才真的居中。 */}
        <div className="grid w-full grid-cols-[1fr_auto_1fr] items-center gap-3 px-6 py-2">
          <div />
          <TopNav
            items={navItems}
            active={page}
            navLabel={strings.primaryNav}
            onChange={navigate}
          />
          <div className="flex items-center justify-self-end gap-1">
            <ScanButton
              scanning={scan.scanning}
              label={strings.overviewRescan}
              scanningLabel={strings.overviewScanning}
              error={scan.lastError}
              onClick={scan.triggerManual}
            />
            <ThemeToggle mode={mode} onChange={setMode} label={toggleLabel} />
          </div>
        </div>
      </div>

      {/* min-h-0 是必须的：flex 子项默认 min-height:auto，内容多时 main 撑破容器，
       * 滚动就会漏到外层去。 */}
      <main className="min-h-0 w-full flex-1 overflow-y-auto px-6 py-6">
        <div
          role="tabpanel"
          id={`panel-${page}`}
          aria-labelledby={`tab-${page}`}
          tabIndex={0}
          /* h-full 是“页面占满视口不滚”的高度链起点：main 高度确定，面板吃满它，
           * fill 页面（明细）才能一路 flex 到表格内部滚动。 */
          className="h-full"
        >
          <NumberUnitContext.Provider value={numberUnit}>
            <ChartAnimationContext.Provider value={chartAnimation}>
            <LocaleContext.Provider value={locale}>
            <PageView
              page={page}
              strings={strings}
              locale={locale}
              onLocaleChange={onLocaleChange}
              mode={mode}
              onModeChange={setMode}
              accent={accent}
              onAccentChange={setAccent}
              disabledTools={disabledTools}
              onDisabledToolsChange={onDisabledToolsChange}
              inputScope={inputScope}
              onInputScopeChange={onInputScopeChange}
              numberUnit={numberUnit}
              onNumberUnitChange={onNumberUnitChange}
              chartAnimation={chartAnimation}
              onChartAnimationChange={onChartAnimationChange}
              trayEnabled={trayEnabled}
              onTrayEnabledChange={onTrayEnabledChange}
              autostart={autostart}
              onAutostartChange={onAutostartChange}
              scanVersion={scan.dataVersion}
              sessionFocus={sessionFocus}
              onOpenSession={openSession}
              onFocusConsumed={consumeFocus}
            />
            </LocaleContext.Provider>
            </ChartAnimationContext.Provider>
          </NumberUnitContext.Provider>
        </div>
      </main>

      {/* 扫描气泡钉在视口右下角：fixed 定位，放这里只是为了跟着应用生命周期走，
       * 位置与所属布局无关（上层容器的 overflow-clip 关不住 fixed 元素）。 */}
      {toast === null ? null : (
        <ScanToast
          kind={toast.kind}
          title={toastTitle(toast.kind, strings)}
          {...(toast.kind === "done"
            ? { detail: scanToastDetail(toast.report, strings) }
            : toast.kind === "running" && toast.backlog !== null
              ? { detail: scanToastBacklogDetail(toast.backlog, strings) }
              : {})}
        />
      )}
    </div>
  );
}
