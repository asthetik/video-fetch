# 第三方组件声明

Video Fetch（影取）在发版包中捆绑以下独立可执行文件与字体文件，供下载、音视频合并与界面渲染使用。本应用源码采用 Apache-2.0 许可证；下列组件各有独立许可证，使用者须一并遵守。

上述二进制由 `scripts/fetch_sidecars.py` 在构建前下载，不入库；版本固定——yt-dlp 固定于 `scripts/requirements-sidecars.txt`（Dependabot 自动升级），ffmpeg 固定于 `scripts/fetch_sidecars.py` 的 `FFMPEG_VERSION`（手动升级）。实际随包版本以应用「关于」页显示为准。

---

## yt-dlp

| 项目 | 说明 |
|------|------|
| **用途** | 解析视频元数据、执行下载 |
| **上游仓库** | https://github.com/yt-dlp/yt-dlp |
| **许可证** | [The Unlicense](https://github.com/yt-dlp/yt-dlp/blob/master/LICENSE)（公有领域 dedication） |
| **本仓库获取方式** | GitHub Releases 固定版本构建（版本固定于 `scripts/requirements-sidecars.txt`，由 Dependabot 自动升级；`scripts/fetch_sidecars.py` 按宿主机选择资产）：<br>• macOS：`yt-dlp_macos`<br>• Linux x86_64：`yt-dlp_linux`<br>• Linux arm64：`yt-dlp_linux_aarch64`<br>• Windows x64：`yt-dlp.exe`<br>• Windows arm64：`yt-dlp_arm64.exe` |

---

## ffmpeg

| 项目 | 说明 |
|------|------|
| **用途** | 合并 DASH 等分离的音视频流 |
| **上游项目** | https://ffmpeg.org/ |
| **许可证** | FFmpeg 上游以 **LGPL v2.1+** 为主；部分静态构建启用 GPL 组件，此时以 **GPL** 为准。详见上游 [LICENSE](https://git.ffmpeg.org/ffmpeg.git/tree/LICENSE.md) 与各构建说明。 |
| **本仓库获取方式** | 按平台选用第三方静态构建（版本固定于 `scripts/fetch_sidecars.py` 的 `FFMPEG_VERSION`，当前 9.0.1，手动升级）：<br>• **macOS**：https://evermeet.cx/ffmpeg/ — `ffmpeg-9.0.1.zip`（单文件 `ffmpeg`）<br>• **Linux**：https://github.com/BtbN/FFmpeg-Builds — `ffmpeg-n9.0-latest-{linux64\|linuxarm64}-gpl-9.0.tar.xz`<br>• **Windows x64**：https://github.com/BtbN/FFmpeg-Builds — `ffmpeg-n9.0-latest-win64-gpl-9.0.zip`<br>• **Windows arm64**：https://github.com/BtbN/FFmpeg-Builds — `ffmpeg-n9.0-latest-winarm64-gpl-9.0.zip` |

BtbN 构建文件名含 `gpl`，表示包含 GPL 许可组件；若需严格 LGPL 链路，请自行替换为符合要求的 ffmpeg 构建并在设置中使用系统路径。

---

## 字体

应用内嵌以下字体文件（位于 `src/assets/fonts/`），随安装包分发：

| 项目 | 说明 |
|------|------|
| **MiSans** | **用途**：界面正文与控件字体。<br>**上游**：小米官方字体 https://hyperos.mi.com/font/<br>**许可证**：小米官方声明可全球免费商用，允许嵌入软件分发；嵌入使用须注明「使用了 MiSans 字体」。<br>**本仓库获取方式**：官方 `MiSans.zip`（v1.1）中的 ttf 经 fontTools 按「GB2312 全集 + 界面字符」子集化为 woff2，仅含 400/600/700/800 四个实际使用的字重。详见 `src/assets/fonts/MiSans-LICENSE-NOTE.md`。 |
| **Smiley Sans（得意黑）** | **用途**：展示字体（wordmark 与大标题）。<br>**上游**：https://github.com/atelier-anchor/smiley-sans<br>**许可证**：[SIL Open Font License 1.1](https://github.com/atelier-anchor/smiley-sans/blob/main/LICENSE)（全文见 `src/assets/fonts/SmileySans-OFL.txt`）。 |
| **JetBrains Mono** | **用途**：数据等宽字体（清晰度、体积、速度等数字）。<br>**上游**：https://www.jetbrains.com/lp/mono/<br>**许可证**：[SIL Open Font License 1.1](https://github.com/JetBrains/JetBrainsMono/blob/master/OFL.txt)<br>**本仓库获取方式**：npm 包 `@fontsource/jetbrains-mono`（400/500 字重，随构建打包）。 |

界面中的楷体时刻（如完成印章）使用系统楷体回退（macOS「楷体-简」/ Windows「楷体」），不随包分发字体文件。

---

## 说明

- Video Fetch**不**修改上述上游源码；仅随应用分发其官方或社区提供的二进制。
- 许可证全文以上游仓库为准；如有冲突，以上游为准。
- 问题反馈：yt-dlp / ffmpeg 行为请参阅各自文档；Video Fetch 集成问题请在本仓库提交 Issue。
