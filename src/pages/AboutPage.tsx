import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import packageJson from "../../package.json";

const REPO_URL = "https://github.com/asthetik/video-fetch";
const LICENSE_URL = `${REPO_URL}/blob/main/LICENSE`;
const THIRD_PARTY_URL = `${REPO_URL}/blob/main/THIRD_PARTY.md`;
/** Same source as release bumps (`package.json`); Tauri runtime may refine via getVersion(). */
const PKG_VERSION = packageJson.version;

async function openExternal(url: string) {
  try {
    await openUrl(url);
  } catch (err) {
    console.error(err);
  }
}

export function AboutPage() {
  const [version, setVersion] = useState(PKG_VERSION);

  useEffect(() => {
    void getVersion()
      .then(setVersion)
      .catch(() => setVersion(PKG_VERSION));
  }, []);

  return (
    <div className="about-page">
      <h2 className="page-title">关于</h2>

      <section className="settings-section">
        <h3>Video Fetch</h3>
        <p className="about-copy">
          轻量桌面视频下载器；当前支持哔哩哔哩（B 站），下载引擎为 yt-dlp。
        </p>
        <dl className="about-meta">
          <div>
            <dt>作者</dt>
            <dd>asthetik</dd>
          </div>
          <div>
            <dt>版本</dt>
            <dd>{version}</dd>
          </div>
          <div>
            <dt>许可证</dt>
            <dd>
              <button
                type="button"
                className="about-link"
                onClick={() => void openExternal(LICENSE_URL)}
              >
                Apache-2.0
              </button>
            </dd>
          </div>
          <div>
            <dt>源码</dt>
            <dd>
              <button
                type="button"
                className="about-link"
                onClick={() => void openExternal(REPO_URL)}
              >
                {REPO_URL}
              </button>
            </dd>
          </div>
        </dl>
      </section>

      <section className="settings-section">
        <h3>第三方组件</h3>
        <p className="about-copy">
          发版包捆绑 yt-dlp 与 ffmpeg，各有独立许可证。详见仓库说明。
        </p>
        <button
          type="button"
          className="btn btn-sm"
          onClick={() => void openExternal(THIRD_PARTY_URL)}
        >
          打开 THIRD_PARTY.md
        </button>
      </section>
    </div>
  );
}
