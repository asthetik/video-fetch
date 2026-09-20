import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ExternalLink } from "lucide-react";
import packageJson from "../../package.json";
import { logUi } from "../lib/activityLog";
import { AUTHOR_URL, LICENSE_URL, REPO_URL, releaseUrl, THIRD_PARTY_URL } from "../lib/links";
import { api } from "../lib/tauri";
import type { EngineVersions } from "../types";

/** Same source as release bumps (`package.json`); Tauri runtime may refine via getVersion(). */
const PKG_VERSION = packageJson.version;

async function openExternal(url: string) {
  try {
    await openUrl(url);
  } catch (err) {
    logUi("about", `打开外部链接失败: ${err instanceof Error ? err.message : String(err)}`, "warn");
  }
}

interface LinkCardProps {
  label: string;
  value: string;
  hint: string;
  url: string;
}

function LinkCard({ label, value, hint, url }: LinkCardProps) {
  return (
    <button
      type="button"
      className="about-card"
      data-action="open-external"
      onClick={() => void openExternal(url)}
    >
      <span className="about-card-label">{label}</span>
      <span className="about-card-value">{value}</span>
      <span className="about-card-hint">
        {hint}
        <ExternalLink size={13} strokeWidth={2} />
      </span>
    </button>
  );
}

export function AboutPage() {
  const [version, setVersion] = useState(PKG_VERSION);
  const [engineVersions, setEngineVersions] = useState<EngineVersions>();

  useEffect(() => {
    void getVersion()
      .then(setVersion)
      .catch(() => setVersion(PKG_VERSION));
  }, []);

  useEffect(() => {
    let cancelled = false;
    api
      .getEngineVersions()
      .then((v) => {
        if (!cancelled) setEngineVersions(v);
      })
      .catch((err) => {
        logUi("about", `获取引擎版本失败: ${err instanceof Error ? err.message : String(err)}`, "warn");
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <div className="about-page">
      <h2 className="page-title">关于</h2>

      <section className="settings-section">
        <h3 className="brand-title">影取 Video Fetch</h3>
        <p className="about-copy">
          轻量桌面视频下载器；当前支持哔哩哔哩（B 站），下载引擎为 yt-dlp。
        </p>
        <div className="about-cards">
          <LinkCard label="作者" value="asthetik" hint="GitHub 主页" url={AUTHOR_URL} />
          <LinkCard label="版本" value={version} hint="发行说明" url={releaseUrl(version)} />
          <LinkCard label="许可证" value="Apache-2.0" hint="许可证全文" url={LICENSE_URL} />
          <LinkCard label="源码" value="asthetik/video-fetch" hint="GitHub 仓库" url={REPO_URL} />
        </div>
      </section>

      <section className="settings-section">
        <h3 className="settings-section-title">第三方组件</h3>
        <p className="about-copy">
          发版包捆绑 yt-dlp 与 ffmpeg，各有独立许可证。详见仓库说明。
        </p>
        <dl className="about-meta">
          <div>
            <dt>yt-dlp</dt>
            <dd>{engineVersions?.ytDlp}</dd>
          </div>
          <div>
            <dt>ffmpeg</dt>
            <dd>{engineVersions?.ffmpeg}</dd>
          </div>
        </dl>
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
