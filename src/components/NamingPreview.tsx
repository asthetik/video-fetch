import { useEffect, useState } from "react";
import { api } from "../lib/tauri";

const SAMPLE = {
  title: "示例视频标题",
  id: "BV1xx411c7mD",
  uploader: "示例UP主",
  ext: "mp4",
  index: 1,
};

interface NamingPreviewProps {
  template: string;
}

export function NamingPreview({ template }: NamingPreviewProps) {
  const [preview, setPreview] = useState("");
  const usesMetadataTime =
    template.includes("%(upload_date)") || template.includes("%(timestamp");

  useEffect(() => {
    let cancelled = false;
    void api
      .previewName(
        template,
        SAMPLE.title,
        SAMPLE.id,
        SAMPLE.uploader,
        SAMPLE.ext,
        SAMPLE.index,
      )
      .then((name) => {
        if (!cancelled) {
          setPreview(name);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setPreview("—");
        }
      });
    return () => {
      cancelled = true;
    };
  }, [template]);

  return (
    <div className="naming-preview">
      <span className="field-label">文件名预览</span>
      <code className="naming-preview-output">{preview || "…"}</code>
      {usesMetadataTime && (
        <p className="naming-preview-note">
          日期为下载开始时的本地时间；预览随当前时间变化。
        </p>
      )}
    </div>
  );
}
