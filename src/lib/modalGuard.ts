/**
 * Esc 互斥护栏：模态弹层（role="alertdialog"，现唯一实体是 ConfirmDialog）
 * 打开时，下层监听——页面级返回（SessionDetailView）、抽屉（CardActions）、
 * 日期弹层（DateRangePicker）——应让位：弹层自己的监听负责关闭它，一次
 * Esc 只关最上层。查询实时 DOM 而不是传状态：各监听挂在 window/document
 * 上，彼此没有可靠的共享所有者，传状态得层层钻孔还容易漏。
 */
export function modalDialogOpen(): boolean {
  return document.querySelector('[role="alertdialog"]') !== null;
}
