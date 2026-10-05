/** 卡片几何：与 `LayoutWidget` 同形，单独命名是为了让卡片只关心"自己摆在哪"。 */

export interface CardGeometry {
  readonly id: string;
  readonly x: number;
  readonly y: number;
  readonly w: number;
  readonly h: number;
}
