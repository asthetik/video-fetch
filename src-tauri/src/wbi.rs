use crate::error::AppResult;
use md5::{Digest, Md5};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use serde_json::Value;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Same byte set as JS `encodeURIComponent` (keeps A-Za-z0-9 - _ . ! ~ * ' ( )).
const ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

const MIXIN_TAB: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WbiKeys {
    pub img_key: String,
    pub sub_key: String,
}

pub fn mixin_key(keys: &WbiKeys) -> String {
    let raw = format!("{}{}", keys.img_key, keys.sub_key);
    MIXIN_TAB
        .iter()
        .filter_map(|&i| raw.as_bytes().get(i))
        .map(|&b| b as char)
        .collect()
}

pub fn sign_with_wts(keys: &WbiKeys, params: &[(String, String)], wts: &str) -> String {
    let mut pairs: Vec<(String, String)> = params.to_vec();
    pairs.push(("wts".into(), wts.to_string()));
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let mix = mixin_key(keys);
    let mut query = String::new();
    for (k, v) in pairs {
        let encoded = utf8_percent_encode(&v, ENCODE_SET).to_string();
        query.push_str(&k);
        query.push('=');
        query.push_str(&encoded.replace(['!', '\'', '(', ')', '*'], ""));
        query.push('&');
    }
    query.pop();
    let bytes: Vec<u8> = query
        .as_bytes()
        .iter()
        .copied()
        .chain(mix.as_bytes().iter().copied())
        .collect();
    let mut hasher = Md5::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sign(keys: &WbiKeys, params: &[(String, String)]) -> (String, String) {
    let wts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default();
    let w_rid = sign_with_wts(keys, params, &wts);
    (wts, w_rid)
}

pub fn parse_nav_keys(v: &Value) -> AppResult<WbiKeys> {
    let img = v
        .pointer("/data/wbi_img/img_url")
        .and_then(Value::as_str)
        .ok_or_else(|| crate::error::AppError::Message("nav API 缺少 wbi_img.img_url".into()))?;
    let sub = v
        .pointer("/data/wbi_img/sub_url")
        .and_then(Value::as_str)
        .ok_or_else(|| crate::error::AppError::Message("nav API 缺少 wbi_img.sub_url".into()))?;
    let stem = |s: &str| {
        s.rsplit('/')
            .next()
            .unwrap_or("")
            .split('.')
            .next()
            .unwrap_or("")
            .to_string()
    };
    Ok(WbiKeys {
        img_key: stem(img),
        sub_key: stem(sub),
    })
}

pub async fn fetch_keys(
    client: &reqwest::Client,
    cookie_header: Option<&str>,
) -> AppResult<WbiKeys> {
    let mut req = client
        .get("https://api.bilibili.com/x/web-interface/nav")
        .header("User-Agent", crate::bilibili_view::USER_AGENT)
        .header("Referer", "https://www.bilibili.com/");
    if let Some(c) = cookie_header {
        req = req.header("Cookie", c);
    }
    let body: Value = req
        .send()
        .await
        .map_err(|e| crate::error::AppError::Message(format!("nav API 请求失败: {e}")))?
        .json()
        .await
        .map_err(|e| crate::error::AppError::Message(format!("nav API JSON 失败: {e}")))?;
    if body.get("code").and_then(Value::as_i64).unwrap_or(-1) != 0 {
        return Err(crate::error::AppError::Message("nav API code != 0".into()));
    }
    parse_nav_keys(&body)
}

/// In-memory WBI key cache with a 24h TTL; callers retry via `invalidate` on risk-control rejections.
pub struct WbiKeyCache {
    inner: Mutex<Option<(WbiKeys, Instant)>>,
}

impl WbiKeyCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    pub fn invalidate(&self) {
        *self.inner.lock().expect("wbi cache lock poisoned") = None;
    }

    pub async fn get_or_fetch(
        &self,
        client: &reqwest::Client,
        cookie_header: Option<&str>,
    ) -> AppResult<WbiKeys> {
        if let Some((keys, at)) = self.inner.lock().expect("wbi cache lock poisoned").as_ref()
            && at.elapsed() < Duration::from_secs(24 * 3600)
        {
            return Ok(keys.clone());
        }
        let keys = fetch_keys(client, cookie_header).await?;
        *self.inner.lock().expect("wbi cache lock poisoned") = Some((keys.clone(), Instant::now()));
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> WbiKeys {
        WbiKeys {
            img_key: "7cd084941338484aae1ad9425b84077c".into(),
            sub_key: "4932caff0ff746eab6f01bf08b70ac45".into(),
        }
    }

    #[test]
    fn mixin_key_matches_known_vector() {
        assert_eq!(
            mixin_key(&keys()),
            "ea1db124af3c7062474693fa704f4ff8ab4a500c8e7ca0784bf98095b48cd341"
        );
    }

    #[test]
    fn sign_produces_expected_w_rid() {
        let params = vec![
            ("bvid".to_string(), "BV1xx411c7mD".to_string()),
            ("cid".to_string(), "123456".to_string()),
        ];
        assert_eq!(
            sign_with_wts(&keys(), &params, "1700000000"),
            "fdfca650860c38d608d40d516906a86a"
        );
    }

    #[test]
    fn parse_nav_keys_extracts_stems() {
        let v = serde_json::json!({
            "code": 0,
            "data": {
                "wbi_img": {
                    "img_url": "https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png",
                    "sub_url": "https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"
                }
            }
        });
        assert_eq!(parse_nav_keys(&v).unwrap(), keys());
    }
}
