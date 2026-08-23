use crate::error::{AppError, AppResult};
use crate::models::FormatOption;
use crate::wbi::{self, WbiKeys};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub const PLAYURL_URL: &str = "https://api.bilibili.com/x/player/wbi/playurl";

// 4048 = DASH | HDR | 4K | Dolby audio | Dolby Vision | 8K | AV1, i.e. every
// available DASH stream so the quality list matches yt-dlp instead of missing
// AV1 variants when fnval=16 is used.
const FNV_ALL_DASH: &str = "4048";

#[derive(Debug, Clone)]
struct DashVariant {
    height: u32,
    codec: String,
    codec_prefix: String,
    fps: u32,
    tbr_kbps: u32,
    qn: i64,
}

/// support_formats carries qn -> display text only; heights live in dash.video.
fn labels_by_qn(data: &Value) -> HashMap<i64, String> {
    let mut label_by_qn: HashMap<i64, String> = HashMap::new();
    if let Some(formats) = data.get("support_formats").and_then(Value::as_array) {
        for f in formats {
            let Some(qn) = f.get("quality").and_then(Value::as_i64) else {
                continue;
            };
            if let Some(desc) = f
                .get("new_description")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                label_by_qn.insert(qn, desc.to_string());
            }
        }
    }
    label_by_qn
}

fn collect_variants(data: &Value) -> AppResult<Vec<DashVariant>> {
    // Build one option per DASH video variant, straight from the response data.
    let mut variants: Vec<DashVariant> = Vec::new();
    let mut seen: HashSet<(u32, String, u32)> = HashSet::new();
    if let Some(dash) = data
        .get("dash")
        .and_then(|d| d.get("video"))
        .and_then(Value::as_array)
    {
        for entry in dash {
            let Some(height) = entry
                .get("height")
                .and_then(Value::as_u64)
                .map(|h| h as u32)
                .filter(|h| *h > 0)
            else {
                continue;
            };
            let codec = entry
                .get("codecs")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let codec_prefix = codec.split('.').next().unwrap_or("").to_string();
            // An empty prefix would make `vcodec^=` match every codec; skip such
            // entries instead of letting them collide with real variants.
            if codec_prefix.is_empty() {
                continue;
            }
            let fps = entry
                .get("frameRate")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<f32>().ok())
                .map(|f| f.round() as u32)
                .filter(|f| *f > 0)
                .unwrap_or(0);
            let tbr_kbps = entry
                .get("bandwidth")
                .and_then(Value::as_u64)
                .map(|b| (b / 1000) as u32)
                .unwrap_or(0);
            if tbr_kbps == 0 {
                continue;
            }
            if !seen.insert((height, codec_prefix.clone(), tbr_kbps)) {
                continue;
            }
            variants.push(DashVariant {
                height,
                codec,
                codec_prefix,
                fps,
                tbr_kbps,
                qn: entry.get("id").and_then(Value::as_i64).unwrap_or(-1),
            });
        }
    }

    // No DASH means a legacy FLV-only video: report failure so the caller falls
    // back to yt-dlp, which already handles those responses.
    if variants.is_empty() {
        return Err(AppError::Message(
            "playurl response has no DASH video streams".into(),
        ));
    }
    Ok(variants)
}

fn data_of(v: &Value) -> AppResult<&Value> {
    if v.get("code").and_then(Value::as_i64).unwrap_or(-1) != 0 {
        let msg = v
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(AppError::Message(format!("playurl code != 0: {msg}")));
    }
    v.get("data")
        .ok_or_else(|| AppError::Message("playurl response missing data".into()))
}

pub fn parse_playurl_formats(v: &Value) -> AppResult<Vec<FormatOption>> {
    let data = data_of(v)?;
    let label_by_qn = labels_by_qn(data);
    let mut variants = collect_variants(data)?;

    // Within one (height, codec) group, split adjacent bitrates at their midpoint
    // so each option's selector matches exactly one variant. The bands assume
    // yt-dlp reports the same tbr (bandwidth/1000) as playurl; if Bilibili
    // re-encodes after these selectors are cached (TTL 7 days), a band may match
    // a different variant or nothing, in which case the `/best` fallback kicks in.
    variants.sort_by_key(|b| std::cmp::Reverse(b.tbr_kbps));
    let mut groups: HashMap<(u32, String), Vec<u32>> = HashMap::new();
    for var in &variants {
        groups
            .entry((var.height, var.codec_prefix.clone()))
            .or_default()
            .push(var.tbr_kbps);
    }

    let mut kept: Vec<FormatOption> = Vec::new();
    for var in &variants {
        let group = &groups[&(var.height, var.codec_prefix.clone())];
        let idx = group.iter().position(|&t| t == var.tbr_kbps).unwrap_or(0);
        let lower = (idx + 1 < group.len()).then(|| (group[idx] + group[idx + 1]) / 2);
        let upper = (idx > 0).then(|| (group[idx - 1] + group[idx]) / 2);
        let desc = label_by_qn.get(&var.qn).cloned().unwrap_or_else(|| {
            crate::ytdlp::resolution_label(Some(var.height), (var.fps > 0).then_some(var.fps))
        });
        let mut selector = format!(
            "bestvideo[height={}][vcodec^={}]",
            var.height, var.codec_prefix
        );
        if let Some(lo) = lower {
            selector.push_str(&format!("[tbr>{lo}]"));
        }
        if let Some(hi) = upper {
            selector.push_str(&format!("[tbr<{hi}]"));
        }
        selector.push_str("+bestaudio/best");
        kept.push(FormatOption {
            format_id: selector,
            label: format!("{desc} · {} · {}kbps", var.codec, var.tbr_kbps),
            height: Some(var.height),
            fps: (var.fps > 0).then_some(var.fps),
            tbr: Some(var.tbr_kbps as f64),
            hires: false,
        });
    }
    kept.sort_by(|a, b| {
        b.height.cmp(&a.height).then(
            b.tbr
                .partial_cmp(&a.tbr)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    Ok(kept)
}

/// Multi-P: one option per (height, codec) instead of per bitrate variant, since
/// per-page bitrates differ and every page may not carry the same variants. The
/// selector falls back to the same height in any codec, then to `best`.
pub fn parse_multi_page_playurl_formats(v: &Value) -> AppResult<Vec<FormatOption>> {
    let data = data_of(v)?;
    let variants = collect_variants(data)?;

    let mut best: HashMap<(u32, String), (u32, u32, String)> = HashMap::new();
    for var in variants {
        let entry = best
            .entry((var.height, var.codec_prefix.clone()))
            .or_insert_with(|| (var.fps, var.tbr_kbps, var.codec.clone()));
        if var.tbr_kbps > entry.1 {
            *entry = (var.fps.max(entry.0), var.tbr_kbps, var.codec.clone());
        } else {
            entry.0 = entry.0.max(var.fps);
        }
    }

    let mut out: Vec<FormatOption> = best
        .into_iter()
        .map(|((height, prefix), (fps, tbr, codec))| FormatOption {
            format_id: format!(
                "bestvideo[height={height}][vcodec^={prefix}]+bestaudio/bestvideo[height={height}]+bestaudio/best"
            ),
            label: format!(
                "{} · {codec}",
                crate::ytdlp::resolution_label(Some(height), (fps > 0).then_some(fps))
            ),
            height: Some(height),
            fps: (fps > 0).then_some(fps),
            tbr: Some(tbr as f64),
            hires: false,
        })
        .collect();
    out.sort_by(|a, b| {
        b.height.cmp(&a.height).then(
            b.tbr
                .partial_cmp(&a.tbr)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    Ok(out)
}

/// Audio tiers from playurl `dash.audio`, plus Hi-Res in `dash.flac` and Dolby
/// in `dash.dolby`. Highest bitrate first. Lower tiers use `bestaudio[abr<=N]`;
/// the top tier is uncapped `bestaudio`.
pub fn parse_audio_formats(v: &Value) -> AppResult<Vec<FormatOption>> {
    let data = data_of(v)?;
    let mut formats: Vec<FormatOption> = Vec::new();
    let mut seen: HashSet<(String, i64)> = HashSet::new();
    for_each_dash_audio(data, |entry| {
        let abr = entry
            .get("bandwidth")
            .and_then(Value::as_u64)
            .map(|b| (b / 1000) as f64);
        let codecs = entry.get("codecs").and_then(Value::as_str).unwrap_or("");
        if codecs.is_empty() {
            return;
        }
        // Entries without a usable bitrate can't form a tier cap and would
        // render as "0kbps"; skip them instead of falling through to the top.
        let Some(abr) = abr else {
            return;
        };
        let key = (codecs.to_string(), abr.round() as i64);
        if !seen.insert(key) {
            return;
        }
        formats.push(FormatOption {
            format_id: String::new(),
            label: crate::ytdlp::audio_stream_label(codecs, Some(abr)),
            height: None,
            fps: None,
            tbr: Some(abr),
            hires: codecs.to_ascii_lowercase().starts_with("flac"),
        });
    });
    formats.sort_by(|a, b| {
        b.tbr
            .partial_cmp(&a.tbr)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (i, f) in formats.iter_mut().enumerate() {
        f.format_id = if i == 0 {
            "bestaudio".to_string()
        } else {
            format!("bestaudio[abr<={:.0}]", f.tbr.unwrap_or(0.0))
        };
    }
    Ok(formats)
}

fn for_each_dash_audio(data: &Value, mut visit: impl FnMut(&Value)) {
    let Some(dash) = data.get("dash") else {
        return;
    };
    if let Some(arr) = dash.get("audio").and_then(Value::as_array) {
        for entry in arr {
            visit(entry);
        }
    }
    for key in ["flac", "dolby"] {
        let Some(node) = dash.get(key) else {
            continue;
        };
        let audio = node.get("audio").unwrap_or(node);
        if let Some(arr) = audio.as_array() {
            for entry in arr {
                visit(entry);
            }
        } else if audio.is_object() {
            visit(audio);
        }
    }
}

/// Merge per-page (height, codec) options from multi-P sampling. Deduplicates by
/// selector and keeps the highest observed bitrate/fps for sorting.
pub fn merge_multi_page_options(target: &mut Vec<FormatOption>, incoming: Vec<FormatOption>) {
    for option in incoming {
        if let Some(existing) = target.iter_mut().find(|o| o.format_id == option.format_id) {
            existing.tbr = match (existing.tbr, option.tbr) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            existing.fps = match (existing.fps, option.fps) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
        } else {
            target.push(option);
        }
    }
    target.sort_by(|a, b| {
        b.height.cmp(&a.height).then(
            b.tbr
                .partial_cmp(&a.tbr)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
}

pub async fn fetch_formats(
    client: &reqwest::Client,
    keys: &WbiKeys,
    bvid: &str,
    cid: &str,
    cookie_header: Option<&str>,
    multi_page: bool,
) -> AppResult<(Vec<FormatOption>, Vec<FormatOption>)> {
    let params: Vec<(String, String)> = vec![
        ("bvid".into(), bvid.to_string()),
        ("cid".into(), cid.to_string()),
        ("qn".into(), "127".into()),
        ("fnval".into(), FNV_ALL_DASH.into()),
        ("fnver".into(), "0".into()),
        ("fourk".into(), "1".into()),
    ];
    let (wts, w_rid) = wbi::sign(keys, &params);
    let mut req = client
        .get(PLAYURL_URL)
        .query(&[
            ("bvid", bvid),
            ("cid", cid),
            ("qn", "127"),
            ("fnval", FNV_ALL_DASH),
            ("fnver", "0"),
            ("fourk", "1"),
            ("wts", &wts),
            ("w_rid", &w_rid),
        ])
        .header("User-Agent", crate::bilibili_view::USER_AGENT)
        .header("Referer", "https://www.bilibili.com/");
    if let Some(c) = cookie_header {
        req = req.header("Cookie", c);
    }
    let body: Value = req
        .send()
        .await
        .map_err(|e| AppError::Message(format!("playurl 请求失败: {e}")))?
        .json()
        .await
        .map_err(|e| AppError::Message(format!("playurl JSON 失败: {e}")))?;
    let video = if multi_page {
        parse_multi_page_playurl_formats(&body)?
    } else {
        parse_playurl_formats(&body)?
    };
    let audio = parse_audio_formats(&body)?;
    Ok((video, audio))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_dash_variants_with_codec_bands() {
        let v = serde_json::json!({
            "code": 0,
            "data": {
                "accept_quality": [112, 64],
                "support_formats": [
                    {"quality": 112, "format": "hdflv2", "new_description": "1080P 高码率"},
                    {"quality": 80, "format": "hdflv2", "new_description": "1080P 高清"},
                    {"quality": 64, "format": "hdflv2", "new_description": "720P 准高清"}
                ],
                "dash": {
                    "video": [
                        {"id": 112, "width": 1920, "height": 1080, "frameRate": "24.000", "codecs": "avc1.640033", "bandwidth": 4900457},
                        {"id": 80, "width": 1920, "height": 1080, "frameRate": "24.000", "codecs": "avc1.640033", "bandwidth": 3356473},
                        {"id": 112, "width": 1920, "height": 1080, "frameRate": "24.000", "codecs": "hvc1.1.6.L150.90", "bandwidth": 2230036},
                        {"id": 64, "width": 1280, "height": 720, "frameRate": "24.000", "codecs": "avc1.640033", "bandwidth": 1604133}
                    ]
                }
            }
        });
        let formats = parse_playurl_formats(&v).unwrap();
        assert_eq!(formats.len(), 4);
        assert_eq!(formats[0].label, "1080P 高码率 · avc1.640033 · 4900kbps");
        assert!(formats[0].format_id.contains("vcodec^=avc1"));
        assert!(formats[0].format_id.contains("tbr>4128"));
        assert_eq!(formats[1].label, "1080P 高清 · avc1.640033 · 3356kbps");
        assert!(formats[1].format_id.contains("tbr<4128"));
        assert_eq!(
            formats[2].label,
            "1080P 高码率 · hvc1.1.6.L150.90 · 2230kbps"
        );
        assert!(!formats[2].format_id.contains("tbr"));
        assert_eq!(formats[3].label, "720P 准高清 · avc1.640033 · 1604kbps");
    }

    #[test]
    fn rejects_non_zero_code() {
        let v = serde_json::json!({"code": -403, "message": "风控校验失败"});
        assert!(parse_playurl_formats(&v).is_err());
    }

    #[test]
    fn rejects_response_without_dash_streams() {
        let v = serde_json::json!({
            "code": 0,
            "data": {
                "accept_quality": [80, 64, 32],
                "support_formats": [
                    {"quality": 80, "format": "flv", "new_description": "1080P 高清"},
                    {"quality": 64, "format": "flv720", "new_description": "720P 高清"},
                    {"quality": 32, "format": "flv480", "new_description": "480P 标清"}
                ],
                "durl": [{"order": 1, "length": 1000, "size": 1, "url": "https://example.com/1.flv"}]
            }
        });
        assert!(parse_playurl_formats(&v).is_err());
    }

    #[test]
    fn dedupes_identical_variants_and_skips_zero_bandwidth() {
        let v = serde_json::json!({
            "code": 0,
            "data": {
                "support_formats": [],
                "dash": {
                    "video": [
                        {"id": 80, "height": 1080, "frameRate": "24.000", "codecs": "avc1.640033", "bandwidth": 3000000},
                        {"id": 80, "height": 1080, "frameRate": "24.000", "codecs": "avc1.640033", "bandwidth": 3000000},
                        {"id": 64, "height": 720, "frameRate": "24.000", "codecs": "avc1.640033", "bandwidth": 0}
                    ]
                }
            }
        });
        let formats = parse_playurl_formats(&v).unwrap();
        assert_eq!(formats.len(), 1);
        assert_eq!(formats[0].height, Some(1080));
    }

    #[test]
    fn skips_variants_without_codecs() {
        let v = serde_json::json!({
            "code": 0,
            "data": {
                "support_formats": [],
                "dash": {
                    "video": [
                        {"id": 80, "height": 1080, "frameRate": "24.000", "codecs": "avc1.640033", "bandwidth": 3000000},
                        {"id": 64, "height": 720, "frameRate": "24.000", "bandwidth": 1500000}
                    ]
                }
            }
        });
        let formats = parse_playurl_formats(&v).unwrap();
        assert_eq!(formats.len(), 1);
        assert_eq!(formats[0].height, Some(1080));
    }

    #[test]
    fn multi_page_parser_groups_by_height_and_codec_with_fallback() {
        let v = serde_json::json!({
            "code": 0,
            "data": {
                "support_formats": [],
                "dash": {
                    "video": [
                        {"id": 80, "height": 1080, "frameRate": "24.000", "codecs": "avc1.640033", "bandwidth": 3000000},
                        {"id": 80, "height": 1080, "frameRate": "24.000", "codecs": "av01.0.08M.08", "bandwidth": 1200000},
                        {"id": 64, "height": 720, "frameRate": "24.000", "codecs": "avc1.640033", "bandwidth": 1500000}
                    ]
                }
            }
        });
        let formats = parse_multi_page_playurl_formats(&v).unwrap();
        assert_eq!(formats.len(), 3);
        assert_eq!(
            formats[0].format_id,
            "bestvideo[height=1080][vcodec^=avc1]+bestaudio/bestvideo[height=1080]+bestaudio/best"
        );
        assert!(formats[0].label.contains("avc1.640033"));
        assert!(formats[1].format_id.contains("vcodec^=av01"));
        assert_eq!(formats[2].height, Some(720));
    }

    #[test]
    fn parses_audio_formats_from_dash() {
        let v = serde_json::json!({
            "code": 0,
            "data": {
                "dash": {
                    "audio": [
                        {"id": 30216, "bandwidth": 64000, "codecs": "mp4a.40.2"},
                        {"id": 30280, "bandwidth": 192000, "codecs": "mp4a.40.2"},
                        {"id": 30251, "bandwidth": 1011000, "codecs": "flac"}
                    ]
                }
            }
        });
        let formats = parse_audio_formats(&v).unwrap();
        assert_eq!(formats.len(), 3);
        assert_eq!(formats[0].label, "Hi-Res 无损");
        assert_eq!(formats[1].format_id, "bestaudio[abr<=192]");
    }

    #[test]
    fn parses_hires_from_dash_flac_object() {
        let v = serde_json::json!({
            "code": 0,
            "data": {
                "dash": {
                    "audio": [
                        {"id": 30216, "bandwidth": 64000, "codecs": "mp4a.40.2"},
                        {"id": 30280, "bandwidth": 192000, "codecs": "mp4a.40.2"}
                    ],
                    "flac": {
                        "display": true,
                        "audio": {"id": 30251, "bandwidth": 1011000, "codecs": "flac"}
                    }
                }
            }
        });
        let formats = parse_audio_formats(&v).unwrap();
        assert_eq!(formats.len(), 3);
        assert_eq!(formats[0].label, "Hi-Res 无损");
        assert_eq!(formats[0].format_id, "bestaudio");
        assert_eq!(formats[1].format_id, "bestaudio[abr<=192]");
        assert!(formats[0].hires);
        assert!(!formats[1].hires);
    }

    #[test]
    fn skips_audio_entries_without_bandwidth() {
        let v = serde_json::json!({
            "code": 0,
            "data": {
                "dash": {
                    "audio": [
                        {"id": 30216, "bandwidth": 64000, "codecs": "mp4a.40.2"},
                        {"id": 30299, "codecs": "mp4a.40.2"}
                    ]
                }
            }
        });
        let formats = parse_audio_formats(&v).unwrap();
        assert_eq!(formats.len(), 1);
        assert_eq!(formats[0].label, "64kbps AAC");
    }

    #[test]
    fn merge_multi_page_options_dedupes_and_keeps_max_bitrate() {
        let page = |bandwidth: u32| {
            vec![FormatOption {
                format_id: "bestvideo[height=1080][vcodec^=avc1]+bestaudio/bestvideo[height=1080]+bestaudio/best".into(),
                label: "1080p · avc1.640033".into(),
                height: Some(1080),
                fps: Some(24),
                tbr: Some(bandwidth as f64),
                hires: false,
            }]
        };
        let mut observed = page(3000);
        merge_multi_page_options(&mut observed, page(4200));
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].tbr, Some(4200.0));
    }
}
