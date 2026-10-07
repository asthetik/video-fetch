# Video Fetch

<p align="center">
  <img src="docs/images/icon.png" alt="Video Fetch 图标" width="128" />
</p>

<p align="center">
  <a href="README.en.md">English</a> | 简体中文
</p>

<p align="center">
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License" /></a>
  <a href="https://github.com/asthetik/video-fetch/actions/workflows/ci.yml"><img src="https://github.com/asthetik/video-fetch/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI" /></a>
  <a href="https://github.com/asthetik/video-fetch/releases"><img src="https://img.shields.io/github/v/release/asthetik/video-fetch" alt="Release" /></a>
  <a href="https://github.com/asthetik/video-fetch/releases"><img src="https://img.shields.io/github/downloads/asthetik/video-fetch/total" alt="Downloads" /></a>
</p>

Video Fetch（影取）是一款轻量的桌面视频下载器，目前支持哔哩哔哩（B 站）。粘贴链接，一键下载。

<p align="center">
  <img src="docs/images/home.png" alt="Video Fetch 主页：多 P 视频勾选分 P 后下载" width="720" />
</p>

## 它能做什么

- 粘贴 B 站视频链接，自动列出清晰度，选好就能下载
- 多 P（分集）视频可以勾选想下载的集数，一次全部下载
- 粘贴 UP 主空间链接，列出全部投稿，可以搜索、排序，跨页勾选后一次性加入下载队列
- 支持「视频 / 仅音频」两种模式：仅音频模式可选音质档位（64/132/192 kbps AAC、Hi-Res 无损）和输出格式（m4a / mp3 / FLAC，FLAC 仅对 Hi-Res 音源开放）
- 登录 B 站后，可以下载大会员专属清晰度
- 下载排队执行（并发数可在「设置 → 下载」里调整），支持暂停、继续、取消、重试，历史记录随时可查
- 下载组件随安装包提供，装完即用，无需额外配置

## 安装

1. 打开 [Releases](https://github.com/asthetik/video-fetch/releases) 页面
2. 下载对应系统的安装包：
   - macOS（仅 Apple 芯片）：`Video-Fetch-v*-macOS.dmg`
   - Windows x64：`Video-Fetch-v*-Windows-x64.msi` / `.exe`
   - Windows arm64：`Video-Fetch-v*-Windows-arm64.msi` / `.exe`
   - Linux x86_64：`Video-Fetch-v*-Linux-x86_64.AppImage` / `.deb`
   - Linux arm64：`Video-Fetch-v*-Linux-arm64.AppImage` / `.deb`
3. 安装后打开即可

安装包**没有做官方签名**，首次打开时各系统会有提示，按下面操作即可：

### macOS

如果提示「Video Fetch.app is damaged and can’t be opened」，不要担心，这不是安装包损坏，只是系统拦截了未签名应用。把 App 拖到「应用程序」后，打开终端执行：

```bash
xattr -cr "/Applications/Video Fetch.app"
```

然后再打开就可以了。

### Windows

如果出现 SmartScreen「未知发布者」提示，点击「仍要运行」即可。

### Linux

`.AppImage` 一般可以直接运行；如果提示没有执行权限，执行：

```bash
chmod +x Video-Fetch-*-Linux-*.AppImage
```

## 使用

1. 粘贴视频链接，等待解析；粘贴 UP 主空间链接则会列出该 UP 主的全部投稿
2. 选择清晰度（多 P 视频还可以勾选集数）；切到「仅音频」可选音质和输出格式
3. 点击下载

想要大会员清晰度？在应用内登录 B 站，或手动导入 `cookies.txt` 文件。

下载好的文件默认保存在系统下载文件夹，可以在「设置 → 下载 → 保存目录」修改。如果取不到系统下载文件夹，会退回到应用数据文件夹下的 `downloads/`。

## 日志与隐私

- 应用会把你的操作（解析、下载、设置等）记录成本地日志
- 应用内的「日志」页可以查看日志、打开日志目录、清空日志
- 登录信息以 cookies 形式保存在本地文件里（macOS 和 Linux 上会限制为仅本人可读）
- 下载记录和设置也保存在同一个文件夹里

应用数据文件夹的位置：

| 系统 | 位置 |
|------|------|
| macOS | `~/Library/Application Support/app.videofetch.desktop/` |
| Windows | `%APPDATA%\app.videofetch.desktop\` |
| Linux | `~/.local/share/app.videofetch.desktop/` |

---

## 开发

### 技术栈

- 桌面框架 Tauri 2：后端 Rust（edition 2024，最低 1.90），前端 React 19 + TypeScript + Vite
- 下载内核 yt-dlp + ffmpeg，作为 sidecar 子进程随安装包分发（sidecar：随应用打包的外部程序）
- 本地存储 SQLite（下载历史与队列）与日志文件

### 本地环境

- Node.js 24
- Rust 1.90 或更新
- Python 3.14（版本钉在 `.python-version`，用于拉取 sidecar）
- Linux 还需要 Tauri 的系统依赖（Ubuntu / Debian）：libwebkit2gtk-4.1-dev、libappindicator3-dev、librsvg2-dev、patchelf、xdg-utils

### 快速开始

```bash
npm install
python3 scripts/fetch_sidecars.py
npm run tauri dev
```

第二步不能省：它把当前平台需要的 yt-dlp 与 ffmpeg 下载到 `src-tauri/binaries/`，开发态从这里加载。已经装了系统版本想直接复用，可以用 `YT_DLP_PATH` / `FFMPEG_PATH` 指向它们。

### 常用命令

| 命令 | 作用 |
|------|------|
| `npm run tauri dev` | 启动桌面应用（开发模式） |
| `npm run build` | 类型检查并构建前端 |
| `npm run test` | 前端单元测试 |
| `cargo test --manifest-path src-tauri/Cargo.toml` | 后端测试 |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --all` | 格式化后端代码 |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings` | 后端静态检查 |

### 集成 / 压力测试（需要先拉 sidecar）

```bash
# 确定性回归套件（CI 同款，串行执行；改动下载引擎时推送前跑一遍）
cargo test --manifest-path src-tauri/Cargo.toml --features test-utils --test regression -- --test-threads=1

# 长压测（本地，默认 5 分钟；SOAK_SEED 可复现操作序列）
SOAK_MINUTES=10 cargo test --manifest-path src-tauri/Cargo.toml --features test-utils --test soak -- --ignored --nocapture
```

### 推送前门禁

改动下载引擎后推送前，本地跑一遍 CI 同款门禁：fmt + clippy（默认与 `--features test-utils` 两种配置）+ 单测 + 回归套件（串行执行）。

```bash
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo clippy --manifest-path src-tauri/Cargo.toml --features test-utils --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --features test-utils --test regression -- --test-threads=1
```

### 项目结构

```
src/                    前端：pages / components / lib
src-tauri/src/          后端：commands、download、ytdlp、wbi、settings、activity_log 等
src-tauri/binaries/     开发态 sidecar（不入库）
scripts/                sidecar 拉取与校验、发版辅助脚本
.github/workflows/      CI 与发版流程
```

## 许可证

Apache-2.0。详见 [`LICENSE`](./LICENSE)。
