# 第三方组件声明

Video Fetch（影取）在发版包中捆绑以下独立可执行文件，供下载与音视频合并使用。本应用源码采用 Apache-2.0 许可证；下列组件各有独立许可证，使用者须一并遵守。

上述二进制由 `scripts/fetch_sidecars.py` 在构建前下载，不入库。

---

## yt-dlp

| 项目 | 说明 |
|------|------|
| **用途** | 解析视频元数据、执行下载 |
| **上游仓库** | https://github.com/yt-dlp/yt-dlp |
| **许可证** | [The Unlicense](https://github.com/yt-dlp/yt-dlp/blob/master/LICENSE)（公有领域 dedication） |
| **本仓库获取方式** | GitHub Releases 最新构建（由 `scripts/fetch_sidecars.py` 按宿主机选择）：<br>• macOS：`yt-dlp_macos`<br>• Linux x86_64：`yt-dlp_linux`<br>• Linux arm64：`yt-dlp_linux_aarch64`<br>• Windows x64：`yt-dlp.exe`<br>• Windows arm64：`yt-dlp_arm64.exe` |

---

## ffmpeg

| 项目 | 说明 |
|------|------|
| **用途** | 合并 DASH 等分离的音视频流 |
| **上游项目** | https://ffmpeg.org/ |
| **许可证** | FFmpeg 上游以 **LGPL v2.1+** 为主；部分静态构建启用 GPL 组件，此时以 **GPL** 为准。详见上游 [LICENSE](https://git.ffmpeg.org/ffmpeg.git/tree/LICENSE.md) 与各构建说明。 |
| **本仓库获取方式** | 按平台选用第三方静态构建（由 `scripts/fetch_sidecars.py` 按宿主机选择）：<br>• **macOS**：https://evermeet.cx/ffmpeg/（单文件 `ffmpeg`）<br>• **Linux**：https://github.com/BtbN/FFmpeg-Builds — `ffmpeg-master-latest-{linux64\|linuxarm64}-gpl.tar.xz`<br>• **Windows x64**：https://github.com/BtbN/FFmpeg-Builds — `ffmpeg-master-latest-win64-gpl.zip`<br>• **Windows arm64**：https://github.com/BtbN/FFmpeg-Builds — `ffmpeg-master-latest-winarm64-gpl.zip` |

BtbN 构建文件名含 `gpl`，表示包含 GPL 许可组件；若需严格 LGPL 链路，请自行替换为符合要求的 ffmpeg 构建并在设置中使用系统路径。

---

## 说明

- Video Fetch**不**修改上述上游源码；仅随应用分发其官方或社区提供的二进制。
- 许可证全文以上游仓库为准；如有冲突，以上游为准。
- 问题反馈：yt-dlp / ffmpeg 行为请参阅各自文档；Video Fetch 集成问题请在本仓库提交 Issue。
