import { expect, it } from "vitest";
import { modelIcon, sanitizeSvg, toolIcon } from "./brandIcons";
import { TOOL_ITEMS } from "./tools";

// TOOL_ICONS 曾经只映射了 3 个工具，其余全部落空——明细页筛选和 Tag 就没了图标。
it("每个受支持工具都解析到编译期图标，不落回 CDN 远程路径", () => {
  for (const tool of TOOL_ITEMS) {
    const icon = toolIcon(tool.id);
    expect(icon, tool.id).toBeDefined();
    expect(icon?.kind, tool.id).not.toBe("remote");
  }
});

// 智谱的模型图标用蓝色点阵球（公司 logo，与生态内其他工具的显示一致），
// 不是改版后的 Z.ai 单色 Z 字。
it("GLM 模型用智谱点阵球标志", () => {
  expect(modelIcon("glm-4.6")?.label).toBe("Zhipu");
  expect(modelIcon("chatglm-turbo")?.label).toBe("Zhipu");
});

// 真实数据里的缩写/别名模型名：混元的 hy 缩写、Meta 的 muse-spark、星火的
// spark-*、小米 MiMo 与 AgnesAI——当时全落空，明细页和下拉没有图标。
it("别名与缩写模型名解析到厂商图标", () => {
  expect(modelIcon("hy3")?.label).toBe("Hunyuan");
  expect(modelIcon("hy4-preview")?.label).toBe("Hunyuan");
  expect(modelIcon("hunyuan-turbo")?.label).toBe("Hunyuan");
  expect(modelIcon("muse-spark-1.3-contrib")?.label).toBe("Meta");
  expect(modelIcon("spark-4.0")?.label).toBe("Spark");
  expect(modelIcon("mimo-v2.5")?.label).toBe("Xiaomi MiMo");
  expect(modelIcon("agnes-2.5-flash")?.label).toBe("AgnesAI");
});

// 单色厂商（MiMo/AgnesAI 无彩色变体）与彩色厂商（混元/Meta）走各自的渲染路径。
it("新厂商图标按有无彩色变体选渲染路径", () => {
  expect(modelIcon("hy4-preview")?.kind).toBe("url");
  expect(modelIcon("muse-spark-1.3")?.kind).toBe("url");
  expect(modelIcon("mimo-v2.5")?.kind).toBe("inline");
  expect(modelIcon("agnes-2.5-flash")?.kind).toBe("inline");
});

// 内联 SVG 经 dangerouslySetInnerHTML 进 DOM：投毒的 SVG 夹带脚本就能执行，
// 清洗必须剔掉 <script>、on* 事件属性与 javascript: 伪协议，正常图形保留。
it("内联 SVG 清洗剔除脚本与事件注入，保留图形", () => {
  const malicious =
    '<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script>' +
    '<rect width="10" height="10" onload="alert(2)" fill="currentColor"/>' +
    '<a href="javascript:alert(3)"><path d="M0 0h8v8"/></a></svg>';
  const clean = sanitizeSvg(malicious);

  expect(clean).not.toContain("<script");
  expect(clean).not.toContain("alert(1)");
  expect(clean).not.toContain("onload");
  expect(clean).not.toContain("javascript:");
  expect(clean).toContain('width="10"');
  expect(clean).toContain('d="M0 0h8v8"');
});
