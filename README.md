# Video Fetch

<p style="text-align: center">
  <img src="docs/images/icon.png" alt="Video Fetch 图标" width="128" />
</p>

<p style="text-align: center">
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License" /></a>
  <a href="https://github.com/asthetik/video-fetch/actions/workflows/ci.yml"><img src="https://github.com/asthetik/video-fetch/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI" /></a>
  <a href="https://github.com/asthetik/video-fetch/releases"><img src="https://img.shields.io/github/v/release/asthetik/video-fetch" alt="Release" /></a>
  <a href="https://github.com/asthetik/video-fetch/releases"><img src="https://img.shields.io/github/downloads/asthetik/video-fetch/total" alt="Downloads" /></a>
</p>

轻量桌面视频下载器（影取），目前支持哔哩哔哩（B 站）。粘贴链接，一键下载。

<p style="text-align: center">
  <img src="docs/images/home-multipage.png" alt="Video Fetch 主页：多 P 视频勾选分 P 后下载" width="720" />
</p>

## 它能做什么

- 粘贴 B 站视频链接，自动列出清晰度，选好就能下载
- 多 P（分集）视频可以勾选要下载的集数，一次全部下载
- 支持「视频 / 仅音频」切换：仅音频可选音质档位（64/132/192kbps AAC、Hi-Res 无损）与输出格式（m4a / mp3 / FLAC，FLAC 仅 Hi-Res 源可选）
- 登录 B 站后，可以下载大会员专属清晰度
- 下载排队执行，可以取消、重试，历史记录随时可查
- 安装包自带下载组件，装完就能用，不需要额外配置

## 安装

1. 打开 [Releases](https://github.com/asthetik/video-fetch/releases) 页面
2. 下载对应系统的安装包：
   - macOS（仅 Apple 芯片）：`Video-Fetch-v*-macOS.dmg`
   - Windows x64：`Video-Fetch-v*-Windows-x64.msi` / `.exe`
   - Windows arm64：`Video-Fetch-v*-Windows-arm64.msi` / `.exe`
   - Linux：`Video-Fetch-v*-Linux-*.AppImage` / `.deb`
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

1. 粘贴视频链接，等待解析
2. 选择清晰度（多 P 视频还可以勾选集数）；切到「仅音频」可选音质和输出格式
3. 点击下载

想要大会员清晰度？在应用内登录 B 站，或手动导入 `cookies.txt` 文件。

## 日志与隐私

- 应用会把你的操作（解析、下载、设置等）记录成本地日志
- 应用内的「日志」页可以查看、打开日志文件夹、清空日志
- 登录信息保存在本地文件里
- 下载记录和设置也保存在同一个文件夹里

应用数据文件夹的位置：

| 系统 | 位置 |
|------|------|
| macOS | `~/Library/Application Support/app.videofetch.desktop/` |
| Windows | `%APPDATA%\app.videofetch.desktop\` |
| Linux | `~/.local/share/app.videofetch.desktop/` |

## 许可证

Apache-2.0。详见 [`LICENSE`](./LICENSE)。
