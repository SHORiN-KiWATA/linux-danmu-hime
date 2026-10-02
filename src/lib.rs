//! 抓 B 站直播间弹幕的核心库。
//!
//! - [`api`]：buvid、WBI 签名、房间信息、弹幕服务器信息、凭据（复用
//!   `bilibili_live_stream` 脚本的登录缓存）
//! - [`protocol`]：弹幕封包编解码与消息解析
//! - [`client`]：WebSocket 客户端，带认证、心跳、自动重连
//!
//! 显示层（ratatui TUI / layer-shell 浮层）不在这里，见 `src/bin/`。

pub mod api;
pub mod client;
pub mod protocol;

pub use api::{BiliSession, Cookies, DanmuInfo, RoomInfo, load_cached_cookies, parse_room_arg};
pub use client::{ClientEvent, DanmakuClient, DanmakuConnection};
pub use protocol::{Danmaku, DanmakuEvent, Emote, Gift, Interact, Medal, SuperChat};

/// 给命令行工具用的小 runtime：单线程就够。
///
/// 关键是 `thread_stack_size`——tokio 的阻塞池（DNS 解析会走）默认给线程
/// 2MB 栈，一次解析就能把这 2MB 摸进常驻内存；压到 256KB 后 RSS 稳定在
/// 3MB 上下，不再随机多出 2MB。
pub fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .thread_stack_size(256 * 1024)
        .build()
}
