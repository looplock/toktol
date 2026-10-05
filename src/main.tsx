import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

// 字体本地打包（随构建产物一起分发，零 CDN 请求）。
import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";

import App from "./App";
import "./styles/tokens.css";

const container = document.getElementById("root");

if (!container) {
  throw new Error("找不到 #root 挂载点，index.html 与入口脚本不匹配");
}

createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
