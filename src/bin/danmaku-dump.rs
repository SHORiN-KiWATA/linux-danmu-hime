//! 把某个直播间的弹幕打到标准输出——先验证「弹幕能收得到」，显示层随后再说。
//!
//! ```text
//! danmaku-dump <房间号|直播间链接> [--json] [--all] [--no-reconnect] [--cookie "..."]
//! ```
//!
//! 状态信息（连接、断开、错误）走 stderr，消息走 stdout，方便 `| jq` 或重放。

use anyhow::{Context, Result, bail};
use danmu_hime::{
    ClientEvent, Cookies, DanmakuClient, DanmakuEvent, load_cached_cookies, parse_room_arg,
};
use chrono::Local;
use std::io::Write;
use tokio::sync::mpsc;

struct Args {
    room: String,
    json: bool,
    all: bool,
    reconnect: bool,
    cookie: Option<String>,
}

fn usage() -> &'static str {
    "用法: danmaku-dump <房间号|直播间链接> [--json] [--all] [--no-reconnect] [--cookie \"SESSDATA=...\"]"
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        room: String::new(),
        json: false,
        all: false,
        reconnect: true,
        cookie: None,
    };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--json" => args.json = true,
            "--all" => args.all = true,
            "--no-reconnect" => args.reconnect = false,
            "--cookie" => {
                args.cookie = Some(argv.next().context("--cookie 后面要跟 cookie 字符串")?);
            }
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other if other.starts_with('-') => bail!("不认识的参数 {other}\n{}", usage()),
            other => {
                if !args.room.is_empty() {
                    bail!("房间号只能给一个\n{}", usage());
                }
                args.room = other.to_string();
            }
        }
    }
    if args.room.is_empty() {
        bail!("{}", usage());
    }
    Ok(args)
}

fn main() -> Result<()> {
    danmu_hime::runtime()
        .context("创建 tokio runtime 失败")?
        .block_on(run())
}

async fn run() -> Result<()> {
    let args = parse_args()?;
    let room_id = parse_room_arg(&args.room)?;
    let cookies = match &args.cookie {
        Some(raw) => Cookies::from_cookie_string(raw),
        None => load_cached_cookies(),
    };
    if !cookies.is_empty() {
        eprintln!("# 已带上登录 cookie（{} 项）", cookies.0.len());
    } else {
        eprintln!("# 未登录，走匿名连接（能收弹幕，收不了需要登录的房间）");
    }

    let mut client = DanmakuClient::new(room_id, cookies);
    client.reconnect = args.reconnect;

    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(client.run(tx));

    while let Some(event) = rx.recv().await {
        match event {
            DanmakuEvent::Guard(guard) => {
                println!(
                    "[上舰] {} {} ×{} (level={} price={})",
                    guard.uname, guard.gift_name, guard.num, guard.level, guard.price
                );
            }
            ClientEvent::Connecting { attempt } => {
                eprintln!(
                    "# 连接中{}…",
                    if attempt > 0 {
                        format!("（第 {attempt} 次重连）")
                    } else {
                        String::new()
                    }
                );
            }
            ClientEvent::Connected { room } => {
                eprintln!(
                    "# 已连接 房间 {}「{}」 {} 在线 {}",
                    room.room_id,
                    room.title,
                    room.status_text(),
                    room.online
                );
            }
            ClientEvent::Disconnected { reason, retry_in } => {
                eprintln!(
                    "# 断线：{reason}{}",
                    match retry_in {
                        Some(delay) => format!("，{} 秒后重连", delay.as_secs()),
                        None => String::new(),
                    }
                );
            }
            ClientEvent::Fatal { message } => {
                eprintln!("# 放弃重连：{message}");
                std::process::exit(1);
            }
            ClientEvent::Danmaku(event) => {
                if args.json {
                    println!("{}", serde_json::to_string(&event)?);
                } else if let Some(line) = render(&event, args.all) {
                    println!("{}", line);
                }
                std::io::stdout().flush().ok();
            }
        }
    }
    Ok(())
}

fn stamp() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

/// 想要人看的朴素文本；不想显示的类型返回 None。
fn render(event: &DanmakuEvent, all: bool) -> Option<String> {
    let time = stamp();
    Some(match event {
        DanmakuEvent::Danmaku(d) => {
            let mut prefix = String::new();
            if let Some(medal) = &d.medal {
                prefix.push_str(&format!("[{}·{}] ", medal.name, medal.level));
            }
            if d.guard > 0 {
                let guard = ["", "总督", "提督", "舰长"][d.guard.min(3) as usize];
                prefix.push_str(&format!("<{guard}> "));
            }
            format!(
                "[{time}] {prefix}{}({}) {}{}",
                d.uname,
                d.uid,
                d.text,
                level_suffix(d.level)
            )
        }
        DanmakuEvent::Gift(g) => format!(
            "[{time}] ✦ {} 投喂 {} ×{}（单位 {} 电池）",
            g.uname, g.gift_name, g.num, g.price
        ),
        DanmakuEvent::SuperChat(s) => {
            format!("[{time}] ☆ 醒目留言 ¥{} {}：{}", s.price, s.uname, s.text)
        }
        DanmakuEvent::Interact(i) => format!("[{time}] · {} {}", i.uname, i.action_text()),
        DanmakuEvent::Like { uname, text } => format!("[{time}] · {uname} {text}"),
        DanmakuEvent::Watched { num, .. } => format!("[{time}] · 看过 {num}"),
        DanmakuEvent::RoomStats { fans, fans_club } => {
            format!("[{time}] · 粉丝 {fans}（粉丝团 {fans_club}）")
        }
        DanmakuEvent::Popularity(value) => format!("[{time}] · 人气 {value}"),
        DanmakuEvent::LiveStatus { living, .. } => {
            format!("[{time}] · {}", if *living { "开播了" } else { "下播了" })
        }
        DanmakuEvent::RoomChange { title, area } => {
            format!("[{time}] · 房间变了：{title} / {area}")
        }
        DanmakuEvent::Other { cmd, .. } => {
            if !all {
                return None;
            }
            format!("[{time}] · [{cmd}]")
        }
    })
}

fn level_suffix(level: i64) -> String {
    if level > 0 {
        format!("  [UL{level}]")
    } else {
        String::new()
    }
}
