import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// 说明：这里用 vitest 的 defineConfig，它兼容 Vite 配置并额外提供 `test` 字段，
// 因此单文件即可同时服务 `pnpm dev` / `pnpm build` / `pnpm test`，无需第二份配置文件。
export default defineConfig(({ mode }) => ({
  plugins: [react(), tailwindcss()],

  resolve: {
    alias: [
      // 仅测试环境（vitest 的 mode 是 "test"）：把 react 桥接回 node require 缓存。
      // vitest 的 module runner 会把源码里裸导入的 react 求值成第二份实例，而外部化
      // react-dom/server 内部 require 到的是 node 缓存里那份——两份实例导致 SSR
      // 测试报 Invalid hook call（useState 读到 null dispatcher）。桥接后源码与
      // react-dom 共用同一份。
      ...(mode === "test"
        ? [{ find: /^react$/, replacement: "/src/test/react-native-bridge.cjs" }]
        : []),
    ],
  },

  // 只有带这些前缀的变量才会暴露给前端代码。Tauri 在开发/构建时注入 TAURI_ENV_*。
  envPrefix: ["VITE_", "TAURI_ENV_"],

  server: {
    // Tauri 的 devUrl 写死了 21720：端口被占用时必须直接失败，而不是静默换一个端口导致壳连不上。
    port: 21720,
    strictPort: true,
    // Rust 的 target/ 目录高频写入，交给文件监听器会拖垮开发体验。
    watch: { ignored: ["**/target/**"] },
  },

  build: {
    target: "es2022",
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
    rollupOptions: {
      output: {
        // echarts 单独成块：应用代码改动时它不必重新下载，也能避开 500kB 的告警。
        manualChunks: { echarts: ["echarts/core", "echarts/charts", "echarts/components", "echarts/renderers"] },
      },
    },
  },

  // 骨架阶段只有纯逻辑冒烟测试，不需要浏览器环境（保持依赖最少）。
  test: {
    environment: "node",
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
  },

  // 让 Rust 侧的编译输出不被 Vite 清屏冲掉。
  clearScreen: false,
}));
