# 安装发布版：未签名说明与核验

本文解释发布安装包为什么没有代码签名、各平台首次运行会看到什么系统提示，
以及不依赖签名的信任核验手段。截图等首个 release 产物构建出来后补齐。

## 为什么没有签名

代码签名证书按年计费，对个人维护的开源项目现阶段不划算——所以发布产物**未签名**。
这不影响软件本身，但操作系统会在首次运行时出示一次性安全提示：这是预期行为，
不是安装包被篡改。签名方案（Trusted Signing / OV 证书 / macOS 公证）留到有稳定
用户量后再评估，届时本文同步更新。

## Windows：SmartScreen 提示

（截图占位：SmartScreen「Windows 已保护你的电脑」弹窗）

1. 点击弹窗上的**更多信息**。
2. 点击**仍要运行**。

部分杀毒软件可能对未签名安装包误报。不要直接放行——先按下一节核对 SHA256，
一致再运行；不一致立即删除并到 Issues 反馈。

## macOS：Gatekeeper 拦截

（截图占位：Gatekeeper「无法打开，因为无法验证开发者」弹窗）

图形界面：**右键点击** App 选**打开**（直接双击会被拦），再在弹窗里点**打开**。
终端用户可以直接放行隔离标记：

```bash
xattr -cr /Applications/Toktol.app
```

## 核对下载文件（SHA256）

GitHub 对每个 release 产物自动计算并展示 SHA-256 摘要（上传时生成，显示在产物旁）。
下载后在本地算一遍、与页面摘要对照：

```bash
# Windows (PowerShell)
Get-FileHash .\Toktol_0.1.0_x64-setup.exe -Algorithm SHA256

# macOS
shasum -a 256 Toktol_0.1.0_aarch64.dmg

# Linux
shasum -a 256 Toktol_0.1.0_amd64.AppImage
```

本地输出与页面上的摘要一致，即下载完整、未在传输途中被篡改。校验和解决的是
"文件没被改过"；"程序本身可信"靠下面两条。

## 从源码构建

仓库公开，最彻底的信任方式是自己编译：

```bash
pnpm install
pnpm tauri build
```

产物在仓库根的 `target/release/bundle/` 下。本地构建按宿主平台产出全部原生
格式（Windows：NSIS + MSI；macOS：.app + .dmg；Linux：deb + AppImage + rpm）；
发布产物的精确清单由 release 工作流的 `--bundles` 逐平台钉死
（Windows：NSIS；macOS：.app + .dmg；Linux：AppImage + deb）。

## 隐私重申

toktol 只**读取**本机 AI 工具的会话与用量数据，不联网上传、数据不出本机
（红线见 README 的隐私一节）。正因为它要碰本机数据，安装前请务必完成上面的
校验步骤——一个未签名的数据扫描类工具，值得多花一分钟核对哈希。
