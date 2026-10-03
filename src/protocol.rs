//! B 站直播弹幕 WebSocket 协议：打包、解包、消息解析。
//!
//! 封包格式（大端）：
//! ```text
//! | 4B 总长 | 2B 头长(16) | 2B 协议版本 | 4B 操作码 | 4B 序号 | body |
//! ```
//! 协议版本：0/1 是明文 JSON 或整数，2 是 zlib，3 是 brotli——2/3 解压出来
//! 还是同样的封包序列，要递归拆。

use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::io::Read;

pub const OP_HEARTBEAT: u32 = 2;
pub const OP_HEARTBEAT_REPLY: u32 = 3;
pub const OP_MESSAGE: u32 = 5;
pub const OP_AUTH: u32 = 7;
pub const OP_AUTH_REPLY: u32 = 8;

pub const HEADER_LEN: usize = 16;
/// 我们发出去的包用明文（服务端不关心，主要看认证包里要的版本）。
pub const PROTOVER_JSON: u16 = 1;
/// 认证包里向服务端要的压缩版本：0/1 明文、2 zlib、3 brotli。
/// 只要 zlib 就够，省掉整个 brotli 解码器（实测服务端认这个）。
pub const AUTH_PROTOVER: u16 = 2;

#[derive(Debug, Clone)]
pub struct Frame {
    pub protover: u16,
    pub op: u32,
    pub body: Vec<u8>,
}

/// 封一个包（操作码 + 明文 body）。
pub fn encode(op: u32, body: &[u8], protover: u16) -> Vec<u8> {
    let total = (HEADER_LEN + body.len()) as u32;
    let mut buf = Vec::with_capacity(total as usize);
    buf.extend_from_slice(&total.to_be_bytes());
    buf.extend_from_slice(&(HEADER_LEN as u16).to_be_bytes());
    buf.extend_from_slice(&protover.to_be_bytes());
    buf.extend_from_slice(&op.to_be_bytes());
    buf.extend_from_slice(&1u32.to_be_bytes());
    buf.extend_from_slice(body);
    buf
}

/// 从一个（可能粘了多个包的）TCP 数据块里拆出所有包。
pub fn parse_frames(mut data: &[u8]) -> Result<Vec<Frame>> {
    let mut frames = Vec::new();
    while !data.is_empty() {
        if data.len() < HEADER_LEN {
            bail!("封包不完整：只剩 {} 字节连头都放不下", data.len());
        }
        let total = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        let header_len = u16::from_be_bytes(data[4..6].try_into().unwrap()) as usize;
        let protover = u16::from_be_bytes(data[6..8].try_into().unwrap());
        let op = u32::from_be_bytes(data[8..12].try_into().unwrap());
        if header_len < HEADER_LEN || total < header_len || total > data.len() {
            bail!(
                "封包长度不对劲：total={total} header={header_len} 实际={}",
                data.len()
            );
        }
        frames.push(Frame {
            protover,
            op,
            body: data[header_len..total].to_vec(),
        });
        data = &data[total..];
    }
    Ok(frames)
}

/// 把压缩帧展开成明文帧（递归到底）。
pub fn expand(frame: Frame) -> Result<Vec<Frame>> {
    expand_with_depth(frame, 0)
}

fn expand_with_depth(frame: Frame, depth: usize) -> Result<Vec<Frame>> {
    if depth > 4 {
        bail!("压缩层数太深，可能不是弹幕包");
    }
    let plain = match frame.protover {
        0 | 1 => return Ok(vec![frame]),
        2 => {
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(frame.body.as_slice())
                .read_to_end(&mut out)
                .context("zlib 解压弹幕包失败")?;
            out
        }
        // 认证时只要了 zlib；真收到 brotli 说明服务端不认账了，得把依赖加回来。
        3 => bail!("收到 brotli 压缩包，但这个构建只支持 zlib"),
        other => bail!("认不出的协议版本 {other}"),
    };
    let mut frames = Vec::new();
    for inner in parse_frames(&plain)? {
        frames.extend(expand_with_depth(inner, depth + 1)?);
    }
    Ok(frames)
}

// ---------------------------------------------------------------------------
// 消息
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Medal {
    pub level: i64,
    pub name: String,
    pub anchor: String,
}

/// 弹幕里带的 B 站表情（`[dog]` 这种）。现行协议直接在弹幕里带原图 URL，
/// 所以不需要任何名字→表情的对照表，也不用登录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Emote {
    /// 弹幕原文里的那个 token，比如 `[dog]`。
    pub text: String,
    /// 表情原图（B 站 CDN 的 png/gif）。
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Danmaku {
    pub text: String,
    pub uid: i64,
    pub uname: String,
    pub level: i64,
    pub color: i64,
    /// 0 无，1 总督，2 提督，3 舰长。
    pub guard: i64,
    pub medal: Option<Medal>,
    pub ts: i64,
    /// 这条弹幕带的表情（普通文字弹幕没有）。
    pub emote: Option<Emote>,
    /// 头像地址，在弹幕里就带着，不用另调接口。
    pub face: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Gift {
    pub gift_id: i64,
    pub face: Option<String>,
    /// 礼物自带的图标地址（`data.gift_info.img_basic`），不用去拉礼物面板。
    pub img: Option<String>,
    /// '喂食' 或 '赠送'。
    pub action: Option<String>,
    pub uid: i64,
    pub uname: String,
    pub gift_name: String,
    pub num: i64,
    pub coin_type: String,
    pub price: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuperChat {
    pub uid: i64,
    pub uname: String,
    pub text: String,
    pub price: i64,
    pub ts: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Interact {
    pub uid: i64,
    pub uname: String,
    /// 1 进入直播间，2 关注，3 分享。
    pub msg_type: i64,
}

impl Interact {
    pub fn action_text(&self) -> &'static str {
        match self.msg_type {
            1 => "进入直播间",
            2 => "关注了主播",
            3 => "分享了直播间",
            _ => "互动",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DanmakuEvent {
    Danmaku(Danmaku),
    Gift(Gift),
    SuperChat(SuperChat),
    Interact(Interact),
    /// 点赞（LIKE_INFO_V3_CLICK）。
    Like {
        uname: String,
        text: String,
    },
    /// 看过人数变化（WATCHED_CHANGE）。
    Watched {
        num: i64,
        text: String,
    },
    /// 直播间人气值（心跳回包）。
    Popularity(u64),
    /// 粉丝数等房间统计（ROOM_REAL_TIME_MESSAGE_UPDATE）。
    RoomStats {
        fans: i64,
        fans_club: i64,
    },
    /// 开播 / 下播（LIVE / PREPARING）。
    LiveStatus {
        living: bool,
        room_id: i64,
    },
    /// 标题或分区变了（ROOM_CHANGE）。
    RoomChange {
        title: String,
        area: String,
    },
    /// 其它命令，先原样收着方便按需加展示。
    Other {
        cmd: String,
        data: serde_json::Value,
    },
}

/// 解析一条明文弹幕消息（一个 JSON）。
pub fn parse_command(value: &serde_json::Value) -> Option<DanmakuEvent> {
    let cmd = value.get("cmd")?.as_str()?;
    // 新协议里 DANMU_MSG 的字段直接在顶层，老协议和其它命令在 data 里。
    let data = value
        .get("data")
        .filter(|data| !data.is_null())
        .unwrap_or(value);
    // 调试开关：DANMU_HIME_DEBUG_RAW=1 时把弹幕/礼物这类消息原样打出来。
    // （上一版只打在 parse_danmaku 里，礼物走不到那儿，白测了一轮。）
    if std::env::var_os("DANMU_HIME_DEBUG_RAW").is_some()
        && matches!(
            cmd,
            "DANMU_MSG" | "SEND_GIFT" | "SUPER_CHAT_MESSAGE" | "GUARD_BUY" | "INTERACT_WORD"
        )
    {
        eprintln!(
            "[raw-cmd] {cmd} {}",
            serde_json::to_string(value).unwrap_or_default()
        );
    }
    match cmd {
        "DANMU_MSG" => parse_danmaku(data),
        "SEND_GIFT" => Some(DanmakuEvent::Gift(parse_gift(data))),
        "SUPER_CHAT_MESSAGE" => Some(DanmakuEvent::SuperChat(parse_super_chat(data))),
        "INTERACT_WORD" => Some(DanmakuEvent::Interact(parse_interact(data))),
        "LIKE_INFO_V3_CLICK" => Some(DanmakuEvent::Like {
            uname: data
                .get("uname")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?")
                .to_string(),
            text: data
                .get("like_text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("点赞了")
                .to_string(),
        }),
        "WATCHED_CHANGE" => Some(DanmakuEvent::Watched {
            num: data
                .get("num")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
            text: data
                .get("text_small")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }),
        "ROOM_REAL_TIME_MESSAGE_UPDATE" => Some(DanmakuEvent::RoomStats {
            fans: data
                .get("fans")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
            fans_club: data
                .get("fans_club")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
        }),
        "LIVE" => Some(DanmakuEvent::LiveStatus {
            living: true,
            room_id: data
                .get("roomid")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
        }),
        "PREPARING" => Some(DanmakuEvent::LiveStatus {
            living: false,
            room_id: 0,
        }),
        "ROOM_CHANGE" => Some(DanmakuEvent::RoomChange {
            title: data
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            area: data
                .get("area_name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }),
        _ => Some(DanmakuEvent::Other {
            cmd: cmd.to_string(),
            data: data.clone(),
        }),
    }
}

/// 心跳回包是 4 字节大端人气值。
pub fn parse_popularity(body: &[u8]) -> Option<u64> {
    if body.len() < 4 {
        return None;
    }
    Some(u32::from_be_bytes(body[0..4].try_into().ok()?) as u64)
}

/// 字段位置对着 blivedm 的 DanmakuMessage.from_command 抄的（2026 年现行协议）。
fn parse_danmaku(payload: &serde_json::Value) -> Option<DanmakuEvent> {
    // 调试开关：DANMU_HIME_DEBUG_RAW=1 时把每一条弹幕的原始 payload 原样打出来。
    // 用来查「直播间私有表情为什么不显示」——grep 弹幕正文就能找到那一条。
    if std::env::var_os("DANMU_HIME_DEBUG_RAW").is_some() {
        eprintln!(
            "[raw-danmaku] {}",
            serde_json::to_string(payload).unwrap_or_default()
        );
    }
    let info = payload.get("info")?.as_array()?;
    let text = info.get(1)?.as_str()?.to_string();
    let meta = info.first().and_then(serde_json::Value::as_array);
    let user_arr = info.get(2).and_then(serde_json::Value::as_array);
    // 老协议把用户名放在 info[0][15].user 里，留着兼容旧回放数据。
    let legacy_user = meta
        .and_then(|m| m.get(15))
        .and_then(|v| v.get("user"))
        .filter(|v| !v.is_null());
    // 头像：B 站把用户对象塞在 info[0] 的某一格里，下标改过好几次
    // （实测 2026-10 是 info[0][16]，老协议是 info[0][15]），所以整段扫一遍找
    // user.base.face，别写死下标。
    let face = meta
        .and_then(|meta| {
            meta.iter().find_map(|slot| {
                slot.get("user")
                    .and_then(|user| user.get("base"))
                    .and_then(|base| base.get("face"))
                    .and_then(serde_json::Value::as_str)
            })
        })
        .or_else(|| {
            legacy_user
                .and_then(|user| user.get("base"))
                .and_then(|base| base.get("face"))
                .and_then(serde_json::Value::as_str)
        })
        .filter(|face| !face.is_empty())
        .map(str::to_string);

    let uid = user_arr
        .and_then(|a| a.first())
        .and_then(serde_json::Value::as_i64)
        .filter(|uid| *uid > 0)
        .or_else(|| {
            legacy_user
                .and_then(|u| u.get("uid"))
                .and_then(serde_json::Value::as_i64)
        })
        .unwrap_or(0);
    let uname = user_arr
        .and_then(|a| a.get(1))
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .or_else(|| {
            legacy_user
                .and_then(|u| u.pointer("/base/name"))
                .and_then(serde_json::Value::as_str)
        })
        .unwrap_or("?")
        .to_string();
    let level = info
        .get(4)
        .and_then(serde_json::Value::as_array)
        .and_then(|a| a.first())
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    // info[7] = 舰队类型：0 非舰队，1 总督，2 提督，3 舰长。
    let guard = info.get(7).and_then(serde_json::Value::as_i64).unwrap_or(0);
    let color = meta
        .and_then(|m| m.get(3))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0xFFFFFF);
    // info[9].ts 是秒；info[0][4] 是毫秒，兜底时除一下。
    let ts = info
        .get(9)
        .and_then(|v| v.get("ts"))
        .and_then(serde_json::Value::as_i64)
        .or_else(|| {
            meta.and_then(|m| m.get(4))
                .and_then(serde_json::Value::as_i64)
                .map(|ms| ms / 1000)
        })
        .unwrap_or(0);
    let medal = info.get(3).and_then(parse_medal);
    let emote = meta.and_then(|m| parse_emote(m));

    Some(DanmakuEvent::Danmaku(Danmaku {
            face,
        text,
        uid,
        uname,
        level,
        color,
        guard,
        medal,
        ts,
        emote,
    }))
}

/// 从 `info[0]` 的几格里找表情。字段位置各客户端不大一样（有的还是一段 JSON
/// 字符串），所以不认下标：整格扫一遍，认「`[名字]` + 图片 URL」这种组合。
fn parse_emote(meta: &[serde_json::Value]) -> Option<Emote> {
    meta.iter().find_map(scan_emote)
}

fn scan_emote(value: &serde_json::Value) -> Option<Emote> {
    match value {
        // 有的客户端把这一格塞成 JSON 字符串
        serde_json::Value::String(raw) => {
            serde_json::from_str(raw).ok().and_then(|parsed| scan_emote(&parsed))
        }
        serde_json::Value::Array(items) => items.iter().find_map(scan_emote),
        serde_json::Value::Object(map) => {
            let url = map
                .get("url")
                .and_then(serde_json::Value::as_str)
                .filter(|url| is_emote_url(url));
            // 形式一：{"text": "[dog]", "url": "https://…/xxx.png"}
            if let (Some(text), Some(url)) = (
                map.get("text").and_then(serde_json::Value::as_str),
                url,
            ) && is_emote_token(text)
            {
                return Some(Emote {
                    text: text.to_string(),
                    url: url.to_string(),
                });
            }
            // 形式三（官方大表情）：{"emoticon_unique": "official_109", "url": "…",
            //                        "width": 138, "height": 60} —— 没有 text 字段，
            // 拿唯一 id 当名字用（占位字符和下载都按它走）。
            if let (Some(url), Some(unique)) = (
                url,
                map.get("emoticon_unique").and_then(serde_json::Value::as_str),
            ) && !unique.is_empty()
            {
                return Some(Emote {
                    text: format!("[{unique}]"),
                    url: url.to_string(),
                });
            }
            // 形式二：{"emots": {"[dog]": {"url": "https://…/xxx.png"}}}
            for (key, item) in map {
                if is_emote_token(key)
                    && let Some(url) = item.get("url").and_then(serde_json::Value::as_str)
                    && is_emote_url(url)
                {
                    return Some(Emote {
                        text: key.clone(),
                        url: url.to_string(),
                    });
                }
            }
            map.values().find_map(scan_emote)
        }
        _ => None,
    }
}

fn is_emote_token(text: &str) -> bool {
    text.len() <= 32 && text.starts_with('[') && text.ends_with(']')
}

fn is_emote_url(url: &str) -> bool {
    url.starts_with("http")
        && (url.ends_with(".png") || url.ends_with(".gif") || url.contains("/bfs/"))
}

fn parse_medal(value: &serde_json::Value) -> Option<Medal> {
    let medal = value.as_array()?;
    let name = medal.get(1)?.as_str()?.to_string();
    if name.is_empty() {
        return None;
    }
    Some(Medal {
        level: medal
            .first()
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        name,
        anchor: medal
            .get(2)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

fn parse_gift(data: &serde_json::Value) -> Gift {
    let num = data
        .get("num")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(1);
    let price = data
        .get("discount_price")
        .and_then(serde_json::Value::as_i64)
        .filter(|p| *p > 0)
        .or_else(|| data.get("price").and_then(serde_json::Value::as_i64))
        .unwrap_or(0);
    Gift {
        gift_id: data
            .get("giftId")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        face: data
            .get("face")
            .and_then(serde_json::Value::as_str)
            .filter(|face| !face.is_empty())
            .map(str::to_string),
        img: data
            .get("gift_info")
            .and_then(|info| info.get("img_basic"))
            .and_then(serde_json::Value::as_str)
            .filter(|img| !img.is_empty())
            .map(str::to_string),
        action: data
            .get("action")
            .and_then(serde_json::Value::as_str)
            .filter(|action| !action.is_empty())
            .map(str::to_string),
        uid: data
            .get("uid")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        uname: data
            .get("uname")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
            .to_string(),
        gift_name: data
            .get("giftName")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("礼物")
            .to_string(),
        num,
        coin_type: data
            .get("coin_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("gold")
            .to_string(),
        price,
    }
}

fn parse_super_chat(data: &serde_json::Value) -> SuperChat {
    SuperChat {
        uid: data
            .get("uid")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        uname: data
            .pointer("/user_info/uname")
            .or_else(|| data.get("uname"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
            .to_string(),
        text: data
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        price: data
            .get("price")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        ts: data
            .get("start_time")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
    }
}

fn parse_interact(data: &serde_json::Value) -> Interact {
    Interact {
        uid: data
            .get("uid")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        uname: data
            .get("uname")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
            .to_string(),
        msg_type: data
            .get("msg_type")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
    }
}

#[cfg(test)]
mod emote_tests {
    #[test]
    fn gift_carries_its_own_icon() {
        // 照 blivedm 的 GiftMessage.from_command 抄的键名
        let data = serde_json::json!({
            "giftName": "辣条",
            "num": 3,
            "uname": "某位观众",
            "face": "https://i1.hdslb.com/bfs/face/abc.jpg",
            "uid": 9202840,
            "giftId": 1,
            "gift_info": {"img_basic": "https://s1.hdslb.com/bfs/live/d57afb7c.png"},
            "action": "赠送",
            "coin_type": "silver",
            "price": 100
        });
        let gift = parse_gift(&data);
        assert_eq!(gift.gift_name, "辣条");
        assert_eq!(gift.num, 3);
        assert_eq!(gift.gift_id, 1);
        assert_eq!(
            gift.img.as_deref(),
            Some("https://s1.hdslb.com/bfs/live/d57afb7c.png")
        );
        assert_eq!(gift.action.as_deref(), Some("赠送"));
        assert!(gift.face.is_some(), "送礼人的头像也要拿到");
    }

    #[test]
    fn official_big_emote_has_no_text_field() {
        // 用户实测：官方大表情只有 url/width/height + emoticon_unique，没有 text
        let value = serde_json::json!({
            "emoticon_unique": "official_109",
            "height": 60,
            "in_player_area": 1,
            "is_dynamic": 1,
            "url": "http://i0.hdslb.com/bfs/live/7b7a2567ad1520f962ee226df777eaf3ca368fbc.png",
            "width": 138
        });
        let emote = scan_emote(&value).expect("官方大表情也要能认出来");
        assert_eq!(emote.text, "[official_109]");
        assert!(emote.url.ends_with(".png"));
    }

    use super::*;

    fn danmaku_with(extra: serde_json::Value) -> Option<Emote> {
        let payload = serde_json::json!({
            "cmd": "DANMU_MSG",
            "info": [
                [0, 1, 25, 16777215, 1, 0, 0, "", 0, 0, 0, "", 0, extra, {}, 0],
                "[dog]",
                [1234, "某人"]
            ]
        });
        match parse_danmaku(&payload) {
            Some(DanmakuEvent::Danmaku(d)) => d.emote,
            _ => None,
        }
    }

    #[test]
    fn flat_emote_object_is_picked_up() {
        let emote = danmaku_with(serde_json::json!({
            "text": "[dog]",
            "url": "https://i0.hdslb.com/bfs/emote/abc.png"
        }))
        .expect("应该认出来");
        assert_eq!(emote.text, "[dog]");
        assert!(emote.url.ends_with("abc.png"));
    }

    #[test]
    fn emots_map_form_is_picked_up() {
        let emote = danmaku_with(serde_json::json!({
            "emots": {
                "[tv_doge]": { "url": "https://i0.hdslb.com/bfs/emote/tvdoge.png", "is_dynamic": 0 }
            }
        }))
        .expect("map 形式也要认");
        assert_eq!(emote.text, "[tv_doge]");
    }

    #[test]
    fn json_string_form_is_picked_up() {
        let emote = danmaku_with(serde_json::json!(
            "{\"text\":\"[大笑]\",\"url\":\"https://i0.hdslb.com/bfs/emote/laugh.png\"}"
        ))
        .expect("JSON 字符串形式也要认");
        assert_eq!(emote.text, "[大笑]");
    }

    #[test]
    fn plain_danmaku_has_no_emote() {
        assert!(danmaku_with(serde_json::json!({})).is_none());
        assert!(danmaku_with(serde_json::json!({"text": "你好", "url": "https://x/y.png"})).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zlib_compress(data: &[u8]) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::ZlibEncoder;
        use std::io::Write;
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn packet_roundtrip() {
        let raw = encode(OP_MESSAGE, b"hello", PROTOVER_JSON);
        let frames = parse_frames(&raw).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].op, OP_MESSAGE);
        assert_eq!(frames[0].body, b"hello");
    }

    #[test]
    fn sticky_packets_split() {
        let mut raw = encode(OP_AUTH_REPLY, b"{}", PROTOVER_JSON);
        raw.extend(encode(OP_HEARTBEAT_REPLY, &7u32.to_be_bytes(), 1));
        let frames = parse_frames(&raw).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].op, OP_HEARTBEAT_REPLY);
    }

    #[test]
    fn compressed_frames_expand() {
        let inner = encode(OP_MESSAGE, br#"{"cmd":"DANMU_MSG"}"#, PROTOVER_JSON);
        let outer = encode(OP_MESSAGE, &zlib_compress(&inner), 2);
        let mut frames = parse_frames(&outer).unwrap();
        assert_eq!(frames.len(), 1);
        let frame = frames.pop().unwrap();
        let expanded = expand(frame).unwrap();
        assert_eq!(expanded.len(), 1);
        assert_eq!(expanded[0].body, br#"{"cmd":"DANMU_MSG"}"#);
    }

    #[test]
    fn brotli_frame_reports_clear_error() {
        // 认证时只要了 zlib；服务端要真发 brotli，得让人知道该加依赖了。
        let frame = Frame {
            protover: 3,
            op: OP_MESSAGE,
            body: vec![0x00],
        };
        let message = expand(frame).unwrap_err().to_string();
        assert!(message.contains("brotli"), "{message}");
    }

    #[test]
    fn parse_new_danmu_msg() {
        // 真实抓包裁出来的新版 DANMU_MSG：字段在顶层，用户信息在 info[2]。
        let raw = r#"{"cmd":"DANMU_MSG","dm_v2":"","info":[[0,1,25,16777215,1790948983186,617962815,0,"6e47be33",0,0,0,"",0,"{}","{}",{}],"全中",[404339910,"Sw1ft1e",0,0,0,10000,1,""],[5,"毛线团","Anicat",953349,6126494,"",0,12632256,12632256,12632256,0,0,27976358],[34,0,10512625,">50000",0],["",""],0,0,null,{"ct":"19F3CC10","ts":1790948983}]}"#;
        let value: serde_json::Value = serde_json::from_str(raw).unwrap();
        match parse_command(&value) {
            Some(DanmakuEvent::Danmaku(danmaku)) => {
                assert_eq!(danmaku.text, "全中");
                assert_eq!(danmaku.uname, "Sw1ft1e");
                assert_eq!(danmaku.uid, 404339910);
                assert_eq!(danmaku.level, 34);
                assert_eq!(danmaku.color, 16777215);
                assert_eq!(danmaku.ts, 1790948983);
                let medal = danmaku.medal.expect("有粉丝牌");
                assert_eq!(medal.level, 5);
                assert_eq!(medal.name, "毛线团");
                assert_eq!(medal.anchor, "Anicat");
            }
            other => panic!("解析错了: {other:?}"),
        }
    }

    #[test]
    fn parse_watched_change() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{"cmd":"WATCHED_CHANGE","data":{"num":123,"text_small":"123"}}"#,
        )
        .unwrap();
        match parse_command(&value) {
            Some(DanmakuEvent::Watched { num, .. }) => assert_eq!(num, 123),
            other => panic!("解析错了: {other:?}"),
        }
    }
}
