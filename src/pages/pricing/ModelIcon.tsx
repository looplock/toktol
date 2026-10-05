/** 模型厂商图标：与明细页筛选同源（brandIcons 前缀规则）；无匹配不占位。 */

import { BrandIcon } from "../../components/ui/BrandIcon";
import { modelIcon } from "../../lib/brandIcons";

export function ModelIcon({ id, size = 14 }: { readonly id: string; readonly size?: number }) {
  const icon = modelIcon(id);
  return icon === undefined ? null : <BrandIcon icon={icon} size={size} />;
}
