/**
 * 品牌图标渲染。两种来源，两种画法：
 * - 彩色（编译期 URL）→ <img>，保留品牌色；
 * - 单色（编译期 ?raw）→ 源码内联，fill="currentColor" 继承文字色。
 * （教训：单色走 <img> 会变黑块，走 CSS mask 在 mask 加载失败时也会整块露底。）
 * 图标一律装饰性（aria-hidden）。
 */

import type { BrandIconAsset } from "../../lib/brandIcons";

interface BrandIconProps {
  readonly icon: BrandIconAsset;
  /** 像素边长。 */
  readonly size?: number;
}

function InlineSvg({ svg, size }: { readonly svg: string; readonly size: number }) {
  // size-full：内联源不统一——lobehub 是 1em，供应商资产可能写死 width="280"，
  // 一律撑满包裹 span（viewBox 自带等比缩放），font-size 只作兜底。
  return (
    <span
      aria-hidden="true"
      className="inline-flex shrink-0 [&>svg]:block [&>svg]:size-full"
      style={{ fontSize: size, width: size, height: size }}
      dangerouslySetInnerHTML={{ __html: svg }}
    />
  );
}

export function BrandIcon({ icon, size = 14 }: BrandIconProps) {
  if (icon.kind === "inline") {
    return <InlineSvg svg={icon.svg} size={size} />;
  }

  return (
    <img
      src={icon.src}
      alt=""
      aria-hidden="true"
      width={size}
      height={size}
      className="shrink-0"
    />
  );
}
