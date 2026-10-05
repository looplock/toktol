/**
 * 转录里的图片附件：白底图卡（方案 A）——图片内嵌在卡内，底栏一行放
 * 文件名、体积与操作（复制路径 / 打开）。原图经 Tauri asset 协议现读
 * （scope 限定在 WorkBuddy 的两个附件目录）；opencode 的 data URL 内嵌图
 * 直接渲染（CSP img-src 已放行 data:）。缺文件 / 非 Tauri 环境 / 加载失败
 * 退回元数据卡，绝不显示裂图。"打开"走 opener 插件，能力文件把可打开的
 * 路径限定在同一对附件目录。
 */

import { useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import type { Strings } from "../../i18n/strings";
import { formatByteSize } from "../../lib/format";
import type { TranscriptBlock } from "../../lib/api";
import { ImageIcon } from "../../components/ui/icons";

type ImageBlock = Extract<TranscriptBlock, { kind: "image" }>;

/** Tauri 运行时才注入 internals；非运行时（SSR / 测试）convertFileSrc 直接抛错，退回 null。 */
function assetUrl(path: string): string | null {
  try {
    return convertFileSrc(path);
  } catch {
    return null;
  }
}

export function ImageAttachment({
  block,
  strings,
  onAccent = false,
}: {
  readonly block: ImageBlock;
  readonly strings: Strings;
  /** 在强调色气泡内：退回的元数据卡用 on-accent 配色（图卡本体自带白底，不受影响）。 */
  readonly onAccent?: boolean;
}) {
  const [loadFailed, setLoadFailed] = useState(false);
  const [copied, setCopied] = useState(false);
  const sizeText = block.size !== null ? ` · ${formatByteSize(block.size)}` : "";

  // 复制路径只在浏览器剪贴板可用时尝试（Tauri WebView 是安全上下文）；
  // 失败静默——图卡里不值得为它弹错误。
  const copyPath = () => {
    void navigator.clipboard
      .writeText(block.path)
      .then(() => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1500);
      })
      .catch(() => {});
  };

  // "打开"走 opener 插件的 open_path（不引它的 npm 包，invoke 直达命令）；
  // 能力文件把可打开路径限定在两个附件目录，越界路径由插件拒绝。
  const openImage = () => {
    invoke("plugin:opener|open_path", { path: block.path }).catch(() => {});
  };

  // opencode 的图片以 data URL 内嵌，直接渲染。注意宽松判空（!= null）：
  // 旧后端的载荷没有 dataUrl 字段（undefined），严格 !== null 会误走此
  // 分支渲染出无 src 的裂图。
  const dataSrc = block.dataUrl != null && !loadFailed ? block.dataUrl : null;
  // 本地路径型：文件存在且资产协议可用才渲染原图。
  const fileSrc = dataSrc === null && block.exists && !loadFailed ? assetUrl(block.path) : null;
  const src = dataSrc ?? fileSrc;

  if (src === null) {
    // 缺文件 / 环境不支持 / 加载失败：退回元数据卡（无操作可做）。
    if (onAccent) {
      return (
        <div className="w-fit max-w-[85%] rounded-card bg-accent-ink/10 px-3.5 py-3">
          <p className="text-xs text-accent-ink/80">
            {block.exists ? strings.transcriptImage : strings.transcriptImageMissing}
          </p>
          <p className="mt-1 flex items-center gap-1.5 font-mono text-xs break-all text-accent-ink">
            <ImageIcon />
            <span className="truncate">
              {block.filename}
              {sizeText}
            </span>
          </p>
        </div>
      );
    }
    return (
      <div
        className={`w-fit max-w-[85%] rounded-card px-3.5 py-3 ${
          block.exists ? "bg-surface-subtle" : "bg-surface-subtle/60 opacity-75"
        }`}
      >
        <p className="text-xs text-ink-muted">
          {block.exists ? strings.transcriptImage : strings.transcriptImageMissing}
        </p>
        <p className="mt-1 flex items-center gap-1.5 font-mono text-xs break-all text-ink">
          <ImageIcon />
          <span className="truncate">
            {block.filename}
            {sizeText}
          </span>
        </p>
      </div>
    );
  }

  // 图卡（方案 A）：图片 + 底栏（文件名 · 体积 · 操作）。操作只对本地
  // 路径型有意义（data URL 内嵌图没有路径可复制/打开）。
  const hasPath = block.path !== "";
  return (
    <figure className="w-fit max-w-[85%] overflow-hidden rounded-card border border-border bg-surface">
      <img
        src={src}
        alt={block.filename}
        loading="lazy"
        onError={() => setLoadFailed(true)}
        className="block max-h-80 w-auto max-w-full"
      />
      <figcaption className="flex items-center gap-1.5 border-t border-border px-3 py-2 font-mono text-xs text-ink-muted">
        <ImageIcon className="size-3.5 shrink-0" />
        <span className="truncate">
          {block.filename}
          {sizeText}
        </span>
        {hasPath ? (
          <span className="ml-auto flex shrink-0 items-center gap-2.5 pl-2 font-sans">
            <button
              type="button"
              onClick={copyPath}
              className="cursor-pointer text-accent transition-opacity hover:opacity-75"
            >
              {copied ? strings.transcriptImageCopied : strings.transcriptImageCopyPath}
            </button>
            <button
              type="button"
              onClick={openImage}
              className="cursor-pointer text-accent transition-opacity hover:opacity-75"
            >
              {strings.transcriptImageOpen}
            </button>
          </span>
        ) : null}
      </figcaption>
    </figure>
  );
}
