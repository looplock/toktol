// 测试专用：把 react 显式拉回 node require 缓存，保证与 react-dom/server
// 内部 require 到的是同一份实例（vitest module runner 会把源码里的裸导入
// react 重新求值成第二份实例，SSR 测试因此报 Invalid hook call）。
module.exports = require("react");
