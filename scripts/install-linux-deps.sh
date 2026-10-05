#!/usr/bin/env bash
#
# Linux 构建依赖的单一事实源。
#
# CI 检查线（.github/workflows/ci.yml）与发布线（.github/workflows/release.yml）
# 共用本脚本。升级 Tauri 需要增删系统包时，只改这里一处，两条工作流同时生效。
#
# 本地 Linux 开发者也可以直接运行它来搭建构建环境：
#   bash scripts/install-linux-deps.sh
#
# 依据：Tauri 2 的 Linux 前置依赖
# https://v2.tauri.app/start/prerequisites/

set -euo pipefail

if ! command -v apt-get >/dev/null 2>&1; then
  echo "错误：本脚本依赖 apt-get，需要 Debian / Ubuntu 系发行版。" >&2
  exit 1
fi

sudo apt-get update
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev \
  libappindicator3-dev \
  librsvg2-dev \
  patchelf \
  libssl-dev
