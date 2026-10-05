/**
 * 品牌图标：LobeChat 那套（@lobehub/icons-static-svg，MIT），全部编译期按路径
 * import——Vite 按引用打包，950 个文件只有被引的进产物。厂商覆盖要扩 = 升包
 * 版本 + 在映射表登记一行。
 *
 * 陷阱：单色图标的 SVG 写的是 fill="currentColor"。当 <img> 用，currentColor 在
 * img 的独立文档里解析成黑色，暗色主题下就是一块黑饼；CSS mask 也踩过坑（mask
 * 加载失败时整块底色直接露出来）。单色一律源码内联进 DOM，让 currentColor 真正
 * 继承文字色——这也是 lobe-icons React 组件自己的渲染方式。彩色 <img>。
 * （曾经的运行时 CDN 拉取路径已删：本地优先的产品不该为小图标发网络请求；
 * 图标库没有的厂商直接无图标、显示文字，升包补齐。）
 */

import agnesai from "@lobehub/icons-static-svg/icons/agnesai.svg?raw";
import baichuanColor from "@lobehub/icons-static-svg/icons/baichuan-color.svg";
import bedrockColor from "@lobehub/icons-static-svg/icons/bedrock-color.svg";
import baiduColor from "@lobehub/icons-static-svg/icons/baidu-color.svg";
import cerebrasColor from "@lobehub/icons-static-svg/icons/cerebras-color.svg";
import claudeColor from "@lobehub/icons-static-svg/icons/claude-color.svg";
import claudecodeColor from "@lobehub/icons-static-svg/icons/claudecode-color.svg";
import cline from "@lobehub/icons-static-svg/icons/cline.svg?raw";
import codebuddyColor from "@lobehub/icons-static-svg/icons/codebuddy-color.svg";
import codexColor from "@lobehub/icons-static-svg/icons/codex-color.svg";
import cohereColor from "@lobehub/icons-static-svg/icons/cohere-color.svg";
import cursor from "@lobehub/icons-static-svg/icons/cursor.svg?raw";
import deepinfraColor from "@lobehub/icons-static-svg/icons/deepinfra-color.svg";
import deepseekColor from "@lobehub/icons-static-svg/icons/deepseek-color.svg";
import doubaoColor from "@lobehub/icons-static-svg/icons/doubao-color.svg";
import fireworksColor from "@lobehub/icons-static-svg/icons/fireworks-color.svg";
import geminiColor from "@lobehub/icons-static-svg/icons/gemini-color.svg";
import geminicliColor from "@lobehub/icons-static-svg/icons/geminicli-color.svg";
import githubcopilot from "@lobehub/icons-static-svg/icons/githubcopilot.svg?raw";
// Groq 无 -color 变体，单色渲染（源码内联）。
import groq from "@lobehub/icons-static-svg/icons/groq.svg?raw";
// OpenAI 与 Grok 无 -color 变体，单色渲染（源码内联，见文件头注释）。
import grok from "@lobehub/icons-static-svg/icons/grok.svg?raw";
// hunyuan 的 -color 变体是腾讯混元的品牌彩色标（蓝橙双色）。
import hunyuanColor from "@lobehub/icons-static-svg/icons/hunyuan-color.svg";
import internlmColor from "@lobehub/icons-static-svg/icons/internlm-color.svg";
// kimi 的 -color 变体是给深色底设计的：主字形写死 fill="#fff"，浅色主题下
// 只剩蓝色角标可见。改成源码内联 + 白字形换 currentColor——字形随主题、
// 品牌蓝角标保留（?raw 而非 URL 资产，走单色画法）。
import kimiColorRaw from "@lobehub/icons-static-svg/icons/kimi-color.svg?raw";
import metaColor from "@lobehub/icons-static-svg/icons/meta-color.svg";
import microsoftColor from "@lobehub/icons-static-svg/icons/microsoft-color.svg";
import minimaxColor from "@lobehub/icons-static-svg/icons/minimax-color.svg";
import mistralColor from "@lobehub/icons-static-svg/icons/mistral-color.svg";
// 小米 MiMo 与 AgnesAI 只有单色变体（fill="currentColor"）：源码内联吃 currentColor。
import mimo from "@lobehub/icons-static-svg/icons/xiaomimimo.svg?raw";
import nvidiaColor from "@lobehub/icons-static-svg/icons/nvidia-color.svg";
import ollama from "@lobehub/icons-static-svg/icons/ollama.svg?raw";
import openai from "@lobehub/icons-static-svg/icons/openai.svg?raw";
import opencode from "@lobehub/icons-static-svg/icons/opencode.svg?raw";
import openrouterColor from "@lobehub/icons-static-svg/icons/openrouter-color.svg";
import perplexityColor from "@lobehub/icons-static-svg/icons/perplexity-color.svg";
import pi from "@lobehub/icons-static-svg/icons/pi.svg?raw";
import qwenColor from "@lobehub/icons-static-svg/icons/qwen-color.svg";
import roocode from "@lobehub/icons-static-svg/icons/roocode.svg?raw";
import sensenovaColor from "@lobehub/icons-static-svg/icons/sensenova-color.svg";
// 讯飞星火的 -color 变体（品牌彩标）。
import sparkColor from "@lobehub/icons-static-svg/icons/spark-color.svg";
import stepfunColor from "@lobehub/icons-static-svg/icons/stepfun-color.svg";
import togetherColor from "@lobehub/icons-static-svg/icons/together-color.svg";
import yiColor from "@lobehub/icons-static-svg/icons/yi-color.svg";
// 智谱的"官方识别像"是蓝色点阵球（公司 logo）；改版后的 Z.ai 标志是单色 Z 字，
// 小尺寸下辨识度差，模型图标用点阵球（与 WorkBuddy 等生态的显示一致）。
import zhipuColor from "@lobehub/icons-static-svg/icons/zhipu-color.svg";
import zcodeSvg from "../assets/zcode.svg?raw";
import workBuddySvg from "../assets/workbuddy.svg?raw";

// ---------- 映射表：名字 → 图标 ----------

export type BrandIconAsset =
  | {
      /** 彩色图标：资产 URL，<img> 渲染。 */
      readonly kind: "url";
      readonly src: string;
      readonly label: string;
    }
  | {
      /** 单色图标：SVG 源码，内联渲染吃 currentColor。 */
      readonly kind: "inline";
      readonly svg: string;
      readonly label: string;
    };

/** 防御纵深：内联 SVG 经 dangerouslySetInnerHTML 进 DOM，HTML 上下文里的
 * <script> 会执行——依赖若被投毒，图标就是注入点。模块加载时对全部单色源码
 * 做一次静态清洗：剔除 <script>、on* 事件属性与 javascript: 伪协议。彩色图标
 * 走 <img>（img 文档不执行脚本），无需处理。 */
export function sanitizeSvg(svg: string): string {
  return svg
    .replace(/<script\b[\s\S]*?<\/script\s*>/gi, "")
    .replace(/\son[a-z]+\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+)/gi, "")
    .replace(/((?:xlink:)?href\s*=\s*(?:"|'))\s*javascript:[^"']*(?:"|')/gi, "$1");
}

const color = (src: string, label: string): BrandIconAsset => ({
  kind: "url",
  src,
  label,
});

const mono = (svg: string, label: string): BrandIconAsset => ({
  kind: "inline",
  svg: sanitizeSvg(svg),
  label,
});

const ANTHROPIC = color(claudeColor, "Anthropic");
const OPENAI = mono(openai, "OpenAI");
const GOOGLE = color(geminiColor, "Google");
const DEEPSEEK = color(deepseekColor, "DeepSeek");
const ALIBABA = color(qwenColor, "Alibaba");
const MOONSHOT = mono(
  kimiColorRaw.replace(/fill="#fff"/g, 'fill="currentColor"'),
  "Moonshot",
);
const ZHIPU = color(zhipuColor, "Zhipu");
const MINIMAX = color(minimaxColor, "MiniMax");
const MISTRAL = color(mistralColor, "Mistral");
const META = color(metaColor, "Meta");
const XAI = mono(grok, "xAI");
const HUNYUAN = color(hunyuanColor, "Hunyuan");
const SPARK = color(sparkColor, "Spark");
const XIAOMI_MIMO = mono(mimo, "Xiaomi MiMo");
const AGNESAI = mono(agnesai, "AgnesAI");
const DOUBAO = color(doubaoColor, "Doubao");
const BAICHUAN = color(baichuanColor, "Baichuan");
const STEPFUN = color(stepfunColor, "StepFun");
const INTERNLM = color(internlmColor, "InternLM");
const SENSENOVA = color(sensenovaColor, "SenseNova");
const BAIDU = color(baiduColor, "Baidu");
const YI = color(yiColor, "Yi");
const COHERE = color(cohereColor, "Cohere");
const GROQ = mono(groq, "Groq");
const PERPLEXITY = color(perplexityColor, "Perplexity");
const OPENROUTER = color(openrouterColor, "OpenRouter");
const TOGETHER = color(togetherColor, "Together");
const CEREBRAS = color(cerebrasColor, "Cerebras");
const BEDROCK = color(bedrockColor, "Bedrock");
const DEEPINFRA = color(deepinfraColor, "DeepInfra");
const FIREWORKS = color(fireworksColor, "Fireworks");
const NVIDIA = color(nvidiaColor, "NVIDIA");
const MICROSOFT = color(microsoftColor, "Microsoft");
const OLLAMA = mono(ollama, "Ollama");
const COPILOT = mono(githubcopilot, "GitHub Copilot");
const CURSOR = mono(cursor, "Cursor");
const CLINE = mono(cline, "Cline");
const ROOCODE = mono(roocode, "Roo Code");

/** 模型名前缀 → 厂商图标（编译期，随包打进产物）。逐条正则按从特殊到一般排。 */
const MODEL_ICON_RULES: readonly (readonly [RegExp, BrandIconAsset])[] = [
  [/^claude/, ANTHROPIC],
  [/^(gpt|chatgpt|o\d|codex|text-davinci|dall-e|whisper)/, OPENAI],
  [/^gemini/, GOOGLE],
  [/^deepseek/, DEEPSEEK],
  [/^qwen/, ALIBABA],
  [/(^kimi|moonshot)/, MOONSHOT],
  [/(^glm|chatglm)/, ZHIPU],
  [/^minimax/, MINIMAX],
  [/^mistral|codestral/, MISTRAL],
  [/^llama/, META],
  [/^grok/, XAI],
  // 混元：官方名 hunyuan-*，部分网关/本地托管用缩写 hy3、hy4-*。
  [/^hunyuan|^hy\d/, HUNYUAN],
  // muse-spark-* 是 Meta 系模型（contrib 分发名），不归星火。
  [/^muse-spark/, META],
  // 星火：官方 spark-*。
  [/^spark/, SPARK],
  [/^mimo/, XIAOMI_MIMO],
  [/^agnes/, AGNESAI],
  [/^doubao/, DOUBAO],
  [/^baichuan/, BAICHUAN],
  [/^step(-|\d|fun)/, STEPFUN],
  [/^internlm/, INTERNLM],
  [/^sensenova/, SENSENOVA],
  [/^ernie|^wenxin/, BAIDU],
  [/^yi-\d|^yi$/, YI],
  [/^command/, COHERE],
  [/^groq/, GROQ],
  [/^sonar/, PERPLEXITY],
  [/^openrouter/, OPENROUTER],
  [/^together/, TOGETHER],
  [/^cerebras/, CEREBRAS],
  [/^bedrock/, BEDROCK],
  [/^deepinfra/, DEEPINFRA],
  [/^fireworks/, FIREWORKS],
  [/^nvidia/, NVIDIA],
  // Phi 是微软的模型：phi 本身无图标，归并到微软。
  [/^(phi|microsoft)/, MICROSOFT],
];

/** 工具名 → 工具自己的图标（有独立图标就用，没有再退回厂商图标）。
 * 与 tools.ts 的 TOOL_ITEMS 一一对应：ids 同步自 Rust `Tool::ALL`，增删两侧要同步。 */
const TOOL_ICONS: Record<string, BrandIconAsset> = {
  "claude-code": color(claudecodeColor, "Claude Code"),
  codebuddy: color(codebuddyColor, "CodeBuddy"),
  codex: color(codexColor, "Codex"),
  // 不在 TOOL_ITEMS 里，但旧库数据可能出现，保留映射。
  "gemini-cli": color(geminicliColor, "Gemini CLI"),
  dsh: DEEPSEEK,
  grok: XAI,
  opencode: mono(opencode, "OpenCode"),
  pi: mono(pi, "Pi"),
  // 供应商资产：WorkBuddy 是彩色源码内联（Settings 同款画法），ZCode 是 currentColor。
  workbuddy: mono(workBuddySvg, "WorkBuddy"),
  zcode: mono(zcodeSvg, "ZCode"),
  ollama: OLLAMA,
  "github-copilot": COPILOT,
  copilot: COPILOT,
  cursor: CURSOR,
  cline: CLINE,
  "roo-code": ROOCODE,
  roocode: ROOCODE,
};

export function modelIcon(model: string): BrandIconAsset | undefined {
  const id = model.toLowerCase();

  return MODEL_ICON_RULES.find(([pattern]) => pattern.test(id))?.[1];
}

export function toolIcon(tool: string): BrandIconAsset | undefined {
  const key = tool.toLowerCase();

  const own = TOOL_ICONS[key];
  if (own !== undefined) {
    return own;
  }

  return modelIcon(key);
}
