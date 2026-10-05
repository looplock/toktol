import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { stringsFor } from "../i18n/strings";
import type {
  PricingCatalogHint,
  PricingModelRow,
  PricingOverviewPayload,
} from "../lib/api";
import { PricingPage } from "./Pricing";

// 用 react-dom/server 而不是 jsdom：只验"渲染出什么"。数据经 initialOverview
// 缝隙注入（SSR 不跑 effect，IPC 拉取自然被跳过），覆盖左列表（状态徽章、
// 价摘要）与右栏的主操作分态（建议价采纳 / 手填 / 确认映射 / 并入入口）。

const strings = stringsFor();

function hintFixture(overrides: Partial<PricingCatalogHint> = {}): PricingCatalogHint {
  return {
    provider: "deepseek",
    modelId: "deepseek/deepseek-v4",
    displayName: "DeepSeek V4",
    status: "active",
    inputPerMtokMicros: 300_000,
    outputPerMtokMicros: 1_200_000,
    cacheReadPerMtokMicros: 30_000,
    cacheWritePerMtokMicros: null,
    ...overrides,
  };
}

function rowFixture(overrides: Partial<PricingModelRow> = {}): PricingModelRow {
  return {
    id: "deepseek-v4",
    displayName: "DeepSeek V4",
    priceSource: null,
    inputPerMtokMicros: null,
    outputPerMtokMicros: null,
    cacheReadPerMtokMicros: null,
    cacheWritePerMtokMicros: null,
    catalog: null,
    usageCount: 0,
    variants: [],
    mappingSource: null,
    ...overrides,
  };
}

it("定价页空态：空列表提示选择，同步信息为尚未同步", () => {
  const html = renderToStaticMarkup(
    <PricingPage
      strings={strings}
      scanVersion={0}
      initialOverview={{ syncedAt: null, models: [], catalogModels: [] }}
    />,
  );
  expect(html).toContain(strings.pricingSelectHint);
  expect(html).toContain(strings.pricingSyncNever);
  // 没有失败提示。
  expect(html).not.toContain(strings.pricingLoadError);
});

it("左列表：待确认徽章与状态筛选，行内不显示变体数与价格", () => {
  const payload: PricingOverviewPayload = {
    syncedAt: 1_789_200_645_293,
    catalogModels: [],
    models: [
      rowFixture({
        usageCount: 30,
        catalog: hintFixture({ status: "deprecated" }),
      }),
      rowFixture({
        id: "claude-opus-4-6",
        priceSource: "user",
        inputPerMtokMicros: 3_000_000,
        outputPerMtokMicros: 15_000_000,
        mappingSource: "user",
        variants: [{ rawModel: "claude-opus-4-6", source: "user" }],
      }),
    ],
  };
  const html = renderToStaticMarkup(
    <PricingPage strings={strings} scanVersion={0} initialOverview={payload} />,
  );

  // 两个模型都在列表里：有目录建议价 → 待确认徽章；已定价且归属已确认的
  // 行不渲染任何徽章（来源细分不再上界面，无对应文案可断言）。
  expect(html).toContain("deepseek-v4");
  expect(html).toContain(strings.pricingStatusPending);
  expect(html).toContain("claude-opus-4-6");
  // 行内摘要（变体数、↑↓价）已下线：价格只在右栏详情里出现。
  expect(html).not.toContain("$3.00");
  expect(html).not.toContain("$15.00");
  // 状态筛选复用会话页 MultiSelect：默认不选 = 全部，选项在弹层里（打开才
  // 渲染），静态帧只见触发钮文案。
  expect(html).toContain(strings.pricingFilterTitle);
});

it("右栏（未定价 + 目录有建议价）：建议价一行预览 + 采纳主按钮", () => {
  const payload: PricingOverviewPayload = {
    syncedAt: null,
    catalogModels: [],
    models: [
      rowFixture({
        usageCount: 30,
        catalog: hintFixture({ status: "deprecated" }),
      }),
    ],
  };
  const html = renderToStaticMarkup(
    <PricingPage strings={strings} scanVersion={0} initialOverview={payload} />,
  );

  // 建议价压成一行（300000 micros = $0.30），废弃标记与采纳入口在。
  expect(html).toContain(strings.pricingSuggestion);
  expect(html).toContain("$0.30");
  expect(html).toContain("$1.20");
  expect(html).toContain("–");
  expect(html).toContain(strings.pricingAdoptSuggestion);
  expect(html).toContain(strings.pricingDeprecated);
  // 手动兜底链接与变体区。
  expect(html).toContain(strings.pricingManualPrice);
  expect(html).toContain(strings.pricingVariantsTitle);
  // 未定价面板没有改价铅笔（那是已定价的行内操作）。
  expect(html).not.toContain(strings.pricingEdit);
});

it("右栏（已定价 + 映射未确认）：确认映射主按钮与并入入口", () => {
  const payload: PricingOverviewPayload = {
    syncedAt: null,
    catalogModels: [],
    models: [
      rowFixture({
        usageCount: 9,
        priceSource: "catalog",
        mappingSource: "suggested",
        variants: [
          { rawModel: "deepseek/deepseek-v4", source: "auto" },
          { rawModel: "gpt-4-0613", source: "suggested" },
        ],
      }),
      rowFixture({ id: "lonely-model", usageCount: 1 }),
    ],
  };
  const html = renderToStaticMarkup(
    <PricingPage strings={strings} scanVersion={0} initialOverview={payload} />,
  );

  expect(html).toContain("deepseek/deepseek-v4");
  // 变体照常列出，但不再渲染来源徽章；列表徽章为待确认；主按钮确认映射。
  expect(html).toContain("gpt-4-0613");
  expect(html).toContain(strings.pricingConfirmMapping);
  expect(html).toContain(strings.pricingConfirmHint);
  expect(html).toContain(strings.pricingStatusPending);
  // 目录关联信息（已关联 models.dev · 供应商）不再展示；同步按钮的
  // aria-label 也含 models.dev，所以用"· 供应商"这个关联副标题独有的片段。
  expect(html).not.toContain("· deepseek");
  // 已定价面板：改价铅笔 + 变体行内改指向。
  expect(html).toContain(strings.pricingEdit);
  expect(html).toContain(strings.pricingMoveTo);
  // 改名与合并收敛为一个"并入"入口。
  expect(html).toContain(strings.pricingUnifyLink);
  // 入口未展开：搜索框不出现。
  expect(html).not.toContain(strings.pricingUnifySearch);
});

it("右栏（未定价 + 目录没收录）：手填编辑器常开，并入入口在", () => {
  const payload: PricingOverviewPayload = {
    syncedAt: null,
    catalogModels: [
      {
        modelId: "glm-5.3",
        provider: "zhipuai",
        displayName: "GLM 5.3",
        status: "active",
        inputPerMtokMicros: null,
        outputPerMtokMicros: null,
        cacheReadPerMtokMicros: null,
        cacheWritePerMtokMicros: null,
      },
    ],
    models: [rowFixture({ id: "hy4-preview-f", usageCount: 3 })],
  };
  const html = renderToStaticMarkup(
    <PricingPage strings={strings} scanVersion={0} initialOverview={payload} />,
  );

  // 没有关联：四桶手填编辑器直接出现，保存入口与说明可见。
  expect(html).toContain(strings.pricingBucketInput);
  expect(html).toContain(strings.pricingSave);
  expect(html).toContain(strings.pricingManualNote);
  // 并入入口在；未展开时搜索框与候选都不出现。
  expect(html).toContain(strings.pricingUnifyLink);
  expect(html).not.toContain(strings.pricingUnifySearch);
  expect(html).not.toContain("glm-5.3");
});

it("改价面板：复制价格选单按输入过滤目录模型并显示四桶价", () => {
  const payload: PricingOverviewPayload = {
    syncedAt: null,
    catalogModels: [
      {
        modelId: "zhipuai/glm-5.3",
        provider: "zhipuai",
        displayName: "GLM 5.3",
        status: "active",
        inputPerMtokMicros: 1_400_000,
        outputPerMtokMicros: 4_400_000,
        cacheReadPerMtokMicros: null,
        cacheWritePerMtokMicros: null,
      },
      {
        modelId: "deepseek/deepseek-v4",
        provider: "deepseek",
        displayName: "DeepSeek V4",
        status: "active",
        inputPerMtokMicros: 300_000,
        outputPerMtokMicros: 1_200_000,
        cacheReadPerMtokMicros: 30_000,
        cacheWritePerMtokMicros: null,
      },
    ],
    models: [
      rowFixture({
        id: "glm-5.3-flash",
        priceSource: "user",
        inputPerMtokMicros: 75_000,
        outputPerMtokMicros: 250_000,
        mappingSource: "user",
      }),
    ],
  };
  const html = renderToStaticMarkup(
    <PricingPage
      strings={strings}
      scanVersion={0}
      initialOverview={payload}
      initialEditing
      initialCopyQuery="glm"
    />,
  );

  // 选单搜索框在；命中的目录项带文字标签价格（1_400_000 micros = $1.40）与供应商。
  expect(html).toContain(strings.pricingCopyPriceSearch);
  expect(html).toContain("zhipuai/glm-5.3");
  expect(html).toContain(`${strings.pricingBucketInput} $1.40`);
  expect(html).toContain(`${strings.pricingBucketOutput} $4.40`);
  expect(html).toContain("zhipuai");
  // 两桶缓存价都缺：该行不渲染缓存标签（目录没给的价不装样子）。
  expect(html).not.toContain("$0.03");
  // 未命中搜索词的目录项不进选单。
  expect(html).not.toContain("deepseek/deepseek-v4");
});

it("改价面板（搜索词为空）：只有提示语，不出候选列表", () => {
  const payload: PricingOverviewPayload = {
    syncedAt: null,
    catalogModels: [
      {
        modelId: "zhipuai/glm-5.3",
        provider: "zhipuai",
        displayName: "GLM 5.3",
        status: "active",
        inputPerMtokMicros: 1_400_000,
        outputPerMtokMicros: 4_400_000,
        cacheReadPerMtokMicros: null,
        cacheWritePerMtokMicros: null,
      },
    ],
    models: [
      rowFixture({
        id: "glm-5.3-flash",
        priceSource: "user",
        mappingSource: "user",
      }),
    ],
  };
  const html = renderToStaticMarkup(
    <PricingPage
      strings={strings}
      scanVersion={0}
      initialOverview={payload}
      initialEditing
    />,
  );

  expect(html).toContain(strings.pricingCopyPriceSearch);
  expect(html).not.toContain("zhipuai/glm-5.3");
});
