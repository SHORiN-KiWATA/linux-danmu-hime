// 临时工具：在直播间蹲一会儿，把带表情的弹幕打出来（验证解析 + 收 URL）。
use danmu_hime::{ClientEvent, Cookies, DanmakuClient, DanmakuEvent};

fn main() -> anyhow::Result<()> {
    let room: i64 = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "6".into())
        .parse()?;
    let seconds: u64 = std::env::args().nth(2).unwrap_or_else(|| "30".into()).parse()?;
    danmu_hime::runtime()?.block_on(async move {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(
            DanmakuClient::new(room, Cookies::from_cookie_string("")).run(tx),
        );
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
        let mut seen: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
        let mut total = 0u32;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match tokio::time::timeout(left, rx.recv()).await {
                Ok(Some(ClientEvent::Danmaku(DanmakuEvent::Danmaku(d)))) => {
                    total += 1;
                    match &d.emote {
                        Some(emote) => {
                            println!("[表情] {} | {}", d.text, emote.url);
                            seen.insert((emote.text.clone(), emote.url.clone()));
                        }
                        None => {
                            if total <= 12 {
                                println!("[弹幕] {}", d.text);
                            }
                        }
                    }
                }
                Ok(Some(other)) => eprintln!("# 事件 {other:?}"),
                Ok(None) | Err(_) => break,
            }
        }
        eprintln!("# 房间 {room}：{total} 条弹幕，{seen_count} 个不同表情", seen_count = seen.len());
    });
    Ok(())
}
