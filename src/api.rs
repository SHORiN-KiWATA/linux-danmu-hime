//! B 站 HTTP 接口：buvid、WBI 签名、房间信息、弹幕服务器信息、凭据加载。
//!
//! 接口逻辑对着 Miyu 里的 Python 脚本 `bilibili_live_stream` 抄的（它又抄自
//! bili-live-hime），登录凭据直接复用那份脚本的缓存文件，不用再登一次。

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36 Edg/143.0.0.0";

pub const LIVE_BASE: &str = "https://api.live.bilibili.com";
pub const MAIN_BASE: &str = "https://api.bilibili.com";

/// WBI 混淆表，B 站 web 端写死的（`mixinKeyEncTab`）。
const MIXIN_KEY_ENC_TAB: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

/// 尽量按浏览器来：Set-Cookie 里同名的新值覆盖旧值。
#[derive(Debug, Clone, Default)]
pub struct Cookies(pub BTreeMap<String, String>);

impl Cookies {
    pub fn from_cookie_string(raw: &str) -> Self {
        let mut map = BTreeMap::new();
        for pair in raw.split(';') {
            let pair = pair.trim();
            if let Some((name, value)) = pair.split_once('=')
                && !name.trim().is_empty()
                && !value.trim().is_empty()
            {
                map.insert(name.trim().to_string(), value.trim().to_string());
            }
        }
        Self(map)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    fn header_value(&self) -> Option<String> {
        if self.0.is_empty() {
            return None;
        }
        Some(
            self.0
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    fn absorb(&mut self, headers: &reqwest::header::HeaderMap) {
        for raw in headers.get_all(reqwest::header::SET_COOKIE) {
            let Ok(text) = raw.to_str() else { continue };
            let Some(first) = text.split(';').next() else {
                continue;
            };
            let Some((name, value)) = first.split_once('=') else {
                continue;
            };
            let (name, value) = (name.trim(), value.trim());
            if name.is_empty()
                || value.is_empty()
                || matches!(value.to_ascii_lowercase().as_str(), "deleted" | "null")
            {
                continue;
            }
            self.0.insert(name.to_string(), value.to_string());
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoomInfo {
    #[serde(default)]
    pub room_id: i64,
    #[serde(default)]
    pub short_id: i64,
    #[serde(default)]
    pub uid: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub uname: Option<String>,
    #[serde(default)]
    pub live_status: i64,
    #[serde(default)]
    pub online: i64,
    #[serde(default)]
    pub area_name: Option<String>,
}

impl RoomInfo {
    pub fn is_living(&self) -> bool {
        self.live_status == 1
    }

    pub fn status_text(&self) -> &'static str {
        match self.live_status {
            1 => "直播中",
            2 => "轮播中",
            _ => "未开播",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DanmuHost {
    pub host: String,
    #[serde(default)]
    pub wss_port: u16,
    #[serde(default)]
    pub ws_port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DanmuInfo {
    pub token: String,
    #[serde(default)]
    pub host_list: Vec<DanmuHost>,
}

/// 进程级 rustls 加密后端：reqwest 用的是 `rustls-no-provider`，
/// 得自己装一个（选 ring，比 aws-lc-rs 小得多）。重复装无害。
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// 下载一张表情图（不需要登录）。给浮层拉 `[dog]` 这种弹幕表情用。
pub async fn fetch_image(url: &str) -> Result<Vec<u8>> {
    install_crypto_provider();
    let http = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("构建 HTTP client 失败")?;
    let response = http
        .get(url)
        .header("Referer", "https://live.bilibili.com/")
        .send()
        .await
        .with_context(|| format!("请求表情图失败：{url}"))?
        .error_for_status()?;
    Ok(response.bytes().await?.to_vec())
}

/// 一个已带 cookie 的 HTTP 会话。
pub struct BiliSession {
    http: reqwest::Client,
    pub cookies: Cookies,
    wbi: Option<(String, String)>,
    uid: Option<i64>,
}

impl BiliSession {
    pub fn new(cookies: Cookies) -> Result<Self> {
        install_crypto_provider();
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .context("构建 HTTP client 失败")?;
        Ok(Self {
            http,
            cookies,
            wbi: None,
            uid: None,
        })
    }

    pub fn logged_in(&self) -> bool {
        self.cookies.get("SESSDATA").is_some() && self.cookies.get("bili_jct").is_some()
    }

    /// 匿名也必须带 buvid3，否则接口 -352。
    pub async fn ensure_buvid(&mut self) -> Result<()> {
        if self.cookies.get("buvid3").is_some() {
            return Ok(());
        }
        let value = self
            .get_raw(MAIN_BASE, "/x/frontend/finger/spi", &[])
            .await?;
        if let Some(data) = value.get("data") {
            for (from, to) in [("b_3", "buvid3"), ("b_4", "buvid4")] {
                if let Some(v) = data.get(from).and_then(Value::as_str) {
                    self.cookies.0.insert(to.to_string(), v.to_string());
                }
            }
        }
        if self.cookies.get("buvid3").is_none() {
            bail!("拿 buvid3 失败，B 站风控接口没返回 b_3");
        }
        Ok(())
    }

    /// 账号 uid：登录时用 DedeUserID，未登录是 0（弹幕服务器允许匿名只读）。
    pub async fn uid(&mut self) -> i64 {
        if let Some(uid) = self.uid {
            return uid;
        }
        let uid = self
            .cookies
            .get("DedeUserID")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        self.uid = Some(uid);
        uid
    }

    pub async fn room_info(&mut self, room_id: i64) -> Result<RoomInfo> {
        self.ensure_buvid().await?;
        let params = [("room_id".to_string(), room_id.to_string())];
        let value = self
            .get_checked(LIVE_BASE, "/room/v1/Room/get_info", &params)
            .await?;
        serde_json::from_value(value).context("解析房间信息失败")
    }

    pub async fn danmu_info(&mut self, room_id: i64) -> Result<DanmuInfo> {
        self.ensure_buvid().await?;
        let mut params = vec![
            ("id".to_string(), room_id.to_string()),
            ("type".to_string(), "0".to_string()),
        ];
        self.sign_wbi(&mut params).await?;
        let value = self
            .get_checked(LIVE_BASE, "/xlive/web-room/v1/index/getDanmuInfo", &params)
            .await?;
        serde_json::from_value(value).context("解析弹幕服务器信息失败")
    }

    /// 给参数补上 wts / w_rid（WBI 签名），否则 getDanmuInfo 直接 -352。
    async fn sign_wbi(&mut self, params: &mut Vec<(String, String)>) -> Result<()> {
        if self.wbi.is_none() {
            self.wbi = Some(self.fetch_wbi_keys().await?);
        }
        let (img_key, sub_key) = self.wbi.clone().expect("刚写入的");
        let wts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间早于 1970")
            .as_secs()
            .to_string();
        params.push(("wts".to_string(), wts));
        let w_rid = wbi_sign(params, &img_key, &sub_key);
        params.push(("w_rid".to_string(), w_rid));
        Ok(())
    }

    async fn fetch_wbi_keys(&mut self) -> Result<(String, String)> {
        let value = self.get_raw(MAIN_BASE, "/x/web-interface/nav", &[]).await?;
        let img_url = value
            .pointer("/data/wbi_img/img_url")
            .and_then(Value::as_str)
            .context("nav 没返回 wbi_img.img_url（多半是风控）")?;
        let sub_url = value
            .pointer("/data/wbi_img/sub_url")
            .and_then(Value::as_str)
            .context("nav 没返回 wbi_img.sub_url（多半是风控）")?;
        let key_of = |url: &str| -> Result<String> {
            url.rsplit('/')
                .next()
                .and_then(|name| name.split('.').next())
                .map(str::to_string)
                .context("wbi key 解析失败")
        };
        Ok((key_of(img_url)?, key_of(sub_url)?))
    }

    async fn get_checked(
        &mut self,
        base: &str,
        path: &str,
        params: &[(String, String)],
    ) -> Result<Value> {
        let value = self.get_raw(base, path, params).await?;
        let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
        if code != 0 {
            let message = value
                .get("message")
                .or_else(|| value.get("msg"))
                .and_then(Value::as_str)
                .unwrap_or("未知错误");
            bail!("B 站接口 {path} 返回错误 {code}: {message}");
        }
        Ok(value.get("data").cloned().unwrap_or(Value::Null))
    }

    async fn get_raw(
        &mut self,
        base: &str,
        path: &str,
        params: &[(String, String)],
    ) -> Result<Value> {
        let url = format!("{base}{path}");
        // live 接口必须装成“直播间页面发出的请求”：Referer/Origin 给 api.live.bilibili.com
        // 会被 getDanmuInfo 直接挡成 -352（实测）。主站接口反而喜欢 api.bilibili.com。
        let site = if base == LIVE_BASE {
            "https://live.bilibili.com"
        } else {
            base
        };
        let mut request = self
            .http
            .get(&url)
            .header("accept", "*/*")
            .header("origin", site)
            .header("referer", format!("{site}/"));
        if !params.is_empty() {
            request = request.query(params);
        }
        if let Some(cookie) = self.cookies.header_value() {
            request = request.header("cookie", cookie);
        }
        if std::env::var_os("DANMAKU_DEBUG").is_some() {
            let query = params
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&");
            eprintln!("[http] {url}?{query}");
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("请求 {url} 失败"))?;
        self.cookies.absorb(response.headers());
        let status = response.status();
        // 没开 reqwest 的 gzip 特性（不主动要压缩），但 CDN 偶尔还是压 —— 自己兜一下。
        let gzipped = response
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("gzip"));
        let bytes = response.bytes().await.unwrap_or_default();
        let text = if gzipped {
            let mut decoded = Vec::new();
            flate2::read::GzDecoder::new(bytes.as_ref())
                .read_to_end(&mut decoded)
                .with_context(|| format!("{url} gzip 解压失败"))?;
            String::from_utf8_lossy(&decoded).into_owned()
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
        if !status.is_success() {
            bail!(
                "{url} 返回 HTTP {status}: {}",
                text.chars().take(200).collect::<String>()
            );
        }
        serde_json::from_str::<Value>(&text).with_context(|| {
            format!(
                "{url} 返回的不是 JSON: {}",
                text.chars().take(200).collect::<String>()
            )
        })
    }
}

/// WBI 签名：混合 key + 参数按 key 排序 + md5，返回 w_rid。
fn wbi_sign(params: &[(String, String)], img_key: &str, sub_key: &str) -> String {
    let mixin = mixin_key(img_key, sub_key);
    let mut sorted: Vec<(String, String)> = params
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| {
            (
                k.clone(),
                v.chars()
                    .filter(|c| !"'!()*".contains(*c))
                    .collect::<String>(),
            )
        })
        .collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let query = sorted
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{:x}", md5::compute(format!("{query}{mixin}")))
}

fn mixin_key(img_key: &str, sub_key: &str) -> String {
    let raw = format!("{img_key}{sub_key}");
    let bytes = raw.as_bytes();
    MIXIN_KEY_ENC_TAB
        .iter()
        .take(32)
        .filter_map(|&i| bytes.get(i).copied())
        .map(char::from)
        .collect()
}

/// 把命令行给的房间号 / URL 解析成数字 room_id。
pub fn parse_room_arg(input: &str) -> Result<i64> {
    let input = input.trim();
    if input.is_empty() {
        bail!("房间号是空的");
    }
    if let Ok(id) = input.parse::<i64>() {
        return Ok(id);
    }
    // 链接只认 live.bilibili.com，别的站（space、b23.tv 短链）不当房间号用。
    let url = input.trim_end_matches('/');
    if url.contains("://") || url.starts_with("live.bilibili.com") {
        let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        if !host.eq_ignore_ascii_case("live.bilibili.com") {
            bail!("只认 live.bilibili.com 的直播间链接，{host} 不认识");
        }
        if let Some(id) = path
            .split(['/', '?', '#'])
            .find_map(|seg| seg.parse::<i64>().ok())
        {
            return Ok(id);
        }
        bail!("链接里没找到房间号：{input}");
    }
    bail!("认不出房间号 {input:?}，可以直接给数字，或直播间链接 https://live.bilibili.com/<房间号>")
}

/// Python 脚本 `bilibili_live_stream` 的缓存目录，凭据（cookies）放这儿。
pub fn cache_dir() -> PathBuf {
    let root = std::env::var_os("BILIBILI_LIVE_STREAM_HOME")
        .or_else(|| std::env::var_os("BILIBILI_STREAM_KEY_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("MIYU_SCRIPT_CACHE_DIR").map(PathBuf::from))
        .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from))
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join(".cache")
        });
    root.join("bilibili-live-stream")
}

/// 从 Python 脚本的 credentials.json 读 cookie；没有就返回空（匿名也能收弹幕）。
pub fn load_cached_cookies() -> Cookies {
    let candidates = [
        cache_dir().join("credentials.json"),
        cache_dir()
            .with_file_name("bilibili-stream-key")
            .join("credentials.json"),
    ];
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(cookies) = value.get("cookies").and_then(Value::as_object) {
            let map = cookies
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect();
            return Cookies(map);
        }
    }
    Cookies::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixin_key_length_and_known_value() {
        // 用 B 站 wbi 文档里的例子验证混淆表：img+sub 取前 32 位。
        let key = mixin_key(
            "7cd084941338484aae1ad9425b84077c",
            "4932caff0ff746eab6f01bf08b70ac45",
        );
        assert_eq!(key.len(), 32);
        assert_eq!(key, "ea1db124af3c7062474693fa704f4ff8");
    }

    #[test]
    fn wbi_sign_is_stable() {
        let params = vec![
            ("id".to_string(), "1".to_string()),
            ("type".to_string(), "0".to_string()),
            ("wts".to_string(), "1700000000".to_string()),
        ];
        let rid = wbi_sign(
            &params,
            "7cd084941338484aae1ad9425b84077c",
            "4932caff0ff746eab6f01bf08b70ac45",
        );
        assert_eq!(rid.len(), 32);
        assert!(rid.chars().all(|c| c.is_ascii_hexdigit()), "{rid}");
    }

    #[test]
    fn parse_room_arg_accepts_url_and_number() {
        assert_eq!(parse_room_arg("21452505").unwrap(), 21452505);
        assert_eq!(
            parse_room_arg("https://live.bilibili.com/21452505?x=1").unwrap(),
            21452505
        );
        assert_eq!(parse_room_arg("live.bilibili.com/blanc/510").unwrap(), 510);
        assert!(parse_room_arg("https://space.bilibili.com/2").is_err());
        assert!(parse_room_arg("https://live.bilibili.com/").is_err());
    }
}
