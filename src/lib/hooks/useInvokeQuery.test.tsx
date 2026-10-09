/**
 * useInvokeQuery 的 SSR 可验部分：装载状态机的首帧形态。异步行为（deps
 * 重拉、竞态作废、轮询）依赖 effect 执行，本测试环境（node + SSR）不可达
 * ——由各页面测试的注入缝隙间接覆盖。
 */

import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { useInvokeQuery } from "./useInvokeQuery";

function Probe({
  deps,
  initialData,
}: {
  readonly deps: readonly unknown[] | null;
  readonly initialData?: string;
}) {
  const query = useInvokeQuery({
    deps,
    fetch: () => Promise.resolve("fetched"),
    ...(initialData === undefined ? {} : { initialData }),
  });
  return (
    <p data-loading={query.loading ? "1" : "0"}>
      {query.data ?? "∅"}
    </p>
  );
}

it("未注入首帧：处于装载态、无数据", () => {
  const html = renderToStaticMarkup(<Probe deps={["k"]} />);
  expect(html).toContain('data-loading="1"');
  expect(html).toContain("∅");
});

it("注入首帧：注入值就地就绪，不显示装载态", () => {
  const html = renderToStaticMarkup(<Probe deps={["k"]} initialData="seed" />);
  expect(html).toContain('data-loading="0"');
  expect(html).toContain("seed");
});

it("deps 停用（null）：不装载、无装载态", () => {
  const html = renderToStaticMarkup(<Probe deps={null} />);
  expect(html).toContain('data-loading="0"');
  expect(html).toContain("∅");
});
