/**
 * 页面注册表与占位实现。真实内容就绪后逐页替换 PageView 的实现，外壳不动。
 * 两张表按 PageId 建表，加页面忘了补会编译报错；标题复用导航标签，两处不会漂移。
 */

import { Card } from "../components/ui/Card";
import { PageShell } from "../components/PageShell";
import type { MessageKey, Strings } from "../i18n/strings";
import type { InputScope } from "../lib/inputScope";
import type { Locale } from "../lib/locale";
import type { ChartAnimation } from "../lib/chartAnimation";
import type { NumberUnit } from "../lib/numberUnit";
import type { PageId, SessionFocus } from "../lib/routes";
import type { AccentId, ThemeMode } from "../lib/theme";
import { DetailsPage } from "./Details";
import { ConfigPage } from "./Config";
import { GatewayPage } from "./Gateway";
import { OverviewPage } from "./Overview";
import { PricingPage } from "./Pricing";
import { SessionsPage } from "./Sessions";
import { SettingsPage } from "./Settings";

export const PAGE_LABEL_KEYS: Record<PageId, MessageKey> = {
  overview: "navOverview",
  details: "navDetails",
  sessions: "navSessions",
  gateway: "navGateway",
  pricing: "navPricing",
  config: "navConfig",
  settings: "navSettings",
};

// 会话页已是真实实现；其余未实现页沿用总表。
const PAGE_DESCRIPTION_KEYS: Partial<Record<PageId, MessageKey>> = {
  overview: "pageOverviewDesc",
  details: "pageDetailsDesc",
  sessions: "pageSessionsDesc",
  gateway: "pageGatewayDesc",
  pricing: "pagePricingDesc",
  config: "pageConfigDesc",
  settings: "pageSettingsDesc",
};

interface PageViewProps {
  readonly page: PageId;
  readonly strings: Strings;
  readonly locale: Locale;
  readonly onLocaleChange: (locale: Locale) => void;
  readonly mode: ThemeMode;
  readonly onModeChange: (mode: ThemeMode) => void;
  readonly accent: AccentId;
  readonly onAccentChange: (accent: AccentId) => void;
  readonly disabledTools: string[];
  readonly onDisabledToolsChange: (next: string[]) => void;
  /** 明细页 Tokens 列的输入口径（设置页"显示"区）。 */
  readonly inputScope: InputScope;
  readonly onInputScopeChange: (scope: InputScope) => void;
  /** token 数量单位制（设置页"显示"区），context 下发给各页与卡片。 */
  readonly numberUnit: NumberUnit;
  readonly onNumberUnitChange: (unit: NumberUnit) => void;
  /** 图表生长动画偏好（设置页"显示"区），context 下发给所有 EChart。 */
  readonly chartAnimation: ChartAnimation;
  readonly onChartAnimationChange: (animation: ChartAnimation) => void;
  /** 托盘常驻与开机自启动（设置页"后台与托盘"区），事实源在壳/系统。 */
  readonly trayEnabled: boolean;
  readonly onTrayEnabledChange: (enabled: boolean) => void;
  readonly autostart: boolean;
  readonly onAutostartChange: (enabled: boolean) => void;
  /** 自动/手动扫描带来新数据时递增：数据页盯着它重新拉取。 */
  readonly scanVersion: number;
  /** 跨页会话焦点：非 null 时会话页定位并选中该会话，消费完回调置回 null。 */
  readonly sessionFocus: SessionFocus | null;
  /** 明细页点会话 → 跳会话页选中。 */
  readonly onOpenSession: (focus: SessionFocus) => void;
  /** 会话页消费完焦点后置空，避免再次进入时重复定位。 */
  readonly onFocusConsumed: () => void;
}

export function PageView({
  page,
  strings,
  locale,
  onLocaleChange,
  mode,
  onModeChange,
  accent,
  onAccentChange,
  disabledTools,
  onDisabledToolsChange,
  inputScope,
  onInputScopeChange,
  numberUnit,
  onNumberUnitChange,
  chartAnimation,
  onChartAnimationChange,
  trayEnabled,
  onTrayEnabledChange,
  autostart,
  onAutostartChange,
  scanVersion,
  sessionFocus,
  onOpenSession,
  onFocusConsumed,
}: PageViewProps) {
  if (page === "overview") {
    return <OverviewPage strings={strings} disabledTools={disabledTools} scanVersion={scanVersion} />;
  }

  if (page === "details") {
    return (
      <DetailsPage
        strings={strings}
        disabledTools={disabledTools}
        scanVersion={scanVersion}
        inputScope={inputScope}
        onOpenSession={onOpenSession}
      />
    );
  }

  if (page === "gateway") {
    return <GatewayPage strings={strings} />;
  }

  if (page === "settings") {
    return (
      <SettingsPage
        strings={strings}
        locale={locale}
        onLocaleChange={onLocaleChange}
        mode={mode}
        onModeChange={onModeChange}
        accent={accent}
        onAccentChange={onAccentChange}
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
      />
    );
  }

  if (page === "sessions") {
    return (
      <SessionsPage
        strings={strings}
        disabledTools={disabledTools}
        focus={sessionFocus ?? undefined}
        onFocusConsumed={onFocusConsumed}
      />
    );
  }

  if (page === "pricing") {
    return <PricingPage strings={strings} scanVersion={scanVersion} />;
  }

  if (page === "config") {
    return <ConfigPage strings={strings} />;
  }

  const descriptionKey = PAGE_DESCRIPTION_KEYS[page];
  return (
    <PageShell
      title={strings[PAGE_LABEL_KEYS[page]]}
      {...(descriptionKey === undefined
        ? {}
        : { description: strings[descriptionKey] })}
    >
      <Card
        dashed
        padding="none"
        className="px-6 py-16 text-center text-sm text-ink-muted"
      >
        {strings.pagePlaceholder}
      </Card>
    </PageShell>
  );
}
