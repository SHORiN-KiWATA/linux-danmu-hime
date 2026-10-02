//! 弹幕 WebSocket 客户端：连接、认证、心跳、读包、断线重连。
//!
//! 用法见 `DanmakuClient::run`：它把「连接/断开/消息」都塞进一个 channel，
//! 显示层不用关心重连和心跳。

use crate::api::{BiliSession, Cookies, DanmuInfo, RoomInfo, USER_AGENT};
use crate::protocol::{
    self, DanmakuEvent, OP_AUTH, OP_AUTH_REPLY, OP_HEARTBEAT, OP_HEARTBEAT_REPLY, OP_MESSAGE,
    PROTOVER_JSON,
};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use std::collections::VecDeque;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// 重连退避：1s 起步，翻倍，封顶 30s。
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// 一条已经拿到 token 的弹幕连接。
pub struct DanmakuConnection {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_heartbeat: Instant,
    pending: VecDeque<DanmakuEvent>,
    pub room: RoomInfo,
    pub host: String,
}

impl DanmakuConnection {
    pub async fn connect(session: &mut BiliSession, room_id: i64) -> Result<Self> {
        let room = session.room_info(room_id).await?;
        // 弹幕服务器要的是真实房间号，不是短号。
        let real_id = if room.room_id > 0 {
            room.room_id
        } else {
            room_id
        };
        let danmu = session.danmu_info(real_id).await?;
        let uid = session.uid().await;

        let host = pick_host(&danmu).context("getDanmuInfo 没返回任何弹幕服务器")?;
        let url = format!("wss://{}:{}/sub", host.host, host.wss_port);

        let mut request = url
            .clone()
            .into_client_request()
            .context("构造 WebSocket 请求失败")?;
        {
            let headers = request.headers_mut();
            headers.insert("user-agent", USER_AGENT.parse().unwrap());
            headers.insert("origin", "https://live.bilibili.com".parse().unwrap());
            headers.insert("referer", "https://live.bilibili.com/".parse().unwrap());
            if let Some(cookie) = cookie_header(&session.cookies) {
                headers.insert("cookie", cookie.parse().context("cookie 头不合法")?);
            }
        }

        let (mut ws, _) = connect_async(request)
            .await
            .with_context(|| format!("连接弹幕服务器 {url} 失败"))?;

        // buvid 必须带，否则能连上、能收房间广播，但收不到弹幕（照 blivedm 抄的）。
        let buvid = session
            .cookies
            .get("buvid3")
            .unwrap_or_default()
            .to_string();
        let auth = serde_json::json!({
            "uid": uid,
            "roomid": real_id,
            "protover": protocol::AUTH_PROTOVER,
            "platform": "web",
            "type": 2,
            "key": danmu.token,
            "buvid": buvid,
        });
        ws.send(Message::Binary(
            protocol::encode(OP_AUTH, auth.to_string().as_bytes(), PROTOVER_JSON).into(),
        ))
        .await
        .context("发送认证包失败")?;

        let mut pending = VecDeque::new();
        wait_auth_reply(&mut ws, &mut pending).await?;

        Ok(Self {
            ws,
            next_heartbeat: Instant::now() + HEARTBEAT_INTERVAL,
            pending,
            room,
            host: url,
        })
    }

    /// 下一条显示层关心的事件；`None` 表示服务端关掉了连接。
    pub async fn next_event(&mut self) -> Result<Option<DanmakuEvent>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(Some(event));
            }
            let sleep_for = self
                .next_heartbeat
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1));
            tokio::select! {
                _ = sleep(sleep_for) => {
                    self.next_heartbeat = Instant::now() + HEARTBEAT_INTERVAL;
                    self.send_heartbeat().await?;
                }
                message = self.ws.next() => {
                    match message {
                        None => return Ok(None),
                        Some(Err(err)) => return Err(err).context("WebSocket 收包失败"),
                        Some(Ok(Message::Binary(data))) => self.handle_payload(&data)?,
                        Some(Ok(Message::Text(text))) => self.handle_payload(text.as_bytes())?,
                        Some(Ok(Message::Close(frame))) => {
                            bail!("服务端关闭连接: {frame:?}");
                        }
                        Some(Ok(_)) => {}
                    }
                }
            }
        }
    }

    async fn send_heartbeat(&mut self) -> Result<()> {
        self.ws
            .send(Message::Binary(
                protocol::encode(OP_HEARTBEAT, b"[object Object]", PROTOVER_JSON).into(),
            ))
            .await
            .context("发送心跳失败")
    }

    fn handle_payload(&mut self, data: &[u8]) -> Result<()> {
        for frame in protocol::parse_frames(data)? {
            let wire_protover = frame.protover;
            for frame in protocol::expand(frame)? {
                match frame.op {
                    OP_HEARTBEAT_REPLY => {
                        if let Some(popularity) = protocol::parse_popularity(&frame.body) {
                            self.pending.push_back(DanmakuEvent::Popularity(popularity));
                        }
                    }
                    OP_MESSAGE => {
                        if frame.body.is_empty() {
                            continue;
                        }
                        let value: serde_json::Value = match serde_json::from_slice(&frame.body) {
                            Ok(value) => value,
                            Err(_) => continue, // 偶发脏包直接跳过
                        };
                        if std::env::var_os("DANMAKU_DEBUG").is_some() {
                            let cmd = value.get("cmd").and_then(|c| c.as_str()).unwrap_or("?");
                            eprintln!(
                                "[ws] wire_pv={wire_protover} {} 字节 cmd={cmd}",
                                frame.body.len()
                            );
                        }
                        if let Some(path) = std::env::var_os("DANMAKU_DUMP_RAW") {
                            use std::io::Write as _;
                            if let Ok(mut file) = std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(path)
                            {
                                let _ = writeln!(file, "{}", String::from_utf8_lossy(&frame.body));
                            }
                        }
                        if let Some(event) = protocol::parse_command(&value) {
                            self.pending.push_back(event);
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

async fn wait_auth_reply(
    ws: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
    pending: &mut VecDeque<DanmakuEvent>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let message = tokio::time::timeout_at(deadline, ws.next())
            .await
            .context("等认证回包超时")?
            .context("连接在认证前就断了")?
            .context("认证阶段收包失败")?;
        let data = match message {
            Message::Binary(data) => data,
            Message::Text(text) => text.into(),
            Message::Close(frame) => bail!("认证阶段被服务端关闭: {frame:?}"),
            _ => continue,
        };
        for frame in protocol::parse_frames(&data)? {
            for frame in protocol::expand(frame)? {
                match frame.op {
                    OP_AUTH_REPLY => {
                        let value: serde_json::Value =
                            serde_json::from_slice(&frame.body).unwrap_or_default();
                        let code = value
                            .get("code")
                            .and_then(serde_json::Value::as_i64)
                            .unwrap_or(0);
                        if code != 0 {
                            bail!("弹幕服务器拒绝认证，code={code}（-101 是登录态失效）");
                        }
                        return Ok(());
                    }
                    OP_HEARTBEAT_REPLY => {
                        if let Some(popularity) = protocol::parse_popularity(&frame.body) {
                            pending.push_back(DanmakuEvent::Popularity(popularity));
                        }
                    }
                    OP_MESSAGE => {
                        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&frame.body)
                            && let Some(event) = protocol::parse_command(&value)
                        {
                            pending.push_back(event);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn pick_host(danmu: &DanmuInfo) -> Option<&crate::api::DanmuHost> {
    danmu
        .host_list
        .iter()
        .find(|host| host.wss_port != 0)
        .or_else(|| danmu.host_list.first())
}

fn cookie_header(cookies: &Cookies) -> Option<String> {
    if cookies.is_empty() {
        return None;
    }
    Some(
        cookies
            .0
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

/// 客户端交给显示层的事件，重连细节都在里面消化掉了。
#[derive(Debug, Clone)]
pub enum ClientEvent {
    Connecting {
        attempt: u32,
    },
    Connected {
        room: RoomInfo,
    },
    Danmaku(DanmakuEvent),
    Disconnected {
        reason: String,
        retry_in: Option<Duration>,
    },
    /// 房间本身无法连接（房间号不存在等），不会重连。
    Fatal {
        message: String,
    },
}

pub struct DanmakuClient {
    pub room_id: i64,
    pub cookies: Cookies,
    pub reconnect: bool,
}

impl DanmakuClient {
    pub fn new(room_id: i64, cookies: Cookies) -> Self {
        Self {
            room_id,
            cookies,
            reconnect: true,
        }
    }

    /// 一直跑：断线自动重连，事件从 `tx` 出去。返回时说明要么被要求
    /// 停止（channel 关闭），要么遇到不可重试的错误。
    pub async fn run(self, tx: mpsc::UnboundedSender<ClientEvent>) {
        let mut attempt: u32 = 0;
        let mut backoff = RECONNECT_MIN;
        loop {
            if tx.send(ClientEvent::Connecting { attempt }).is_err() {
                return;
            }
            let result = self.connect_once(&tx).await;
            match result {
                Ok(()) => return,
                Err(err) => {
                    let message = format!("{err:#}");
                    if !self.reconnect {
                        let _ = tx.send(ClientEvent::Fatal { message });
                        return;
                    }
                    attempt += 1;
                    let _ = tx.send(ClientEvent::Disconnected {
                        reason: message,
                        retry_in: Some(backoff),
                    });
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(RECONNECT_MAX);
                }
            }
            if tx.is_closed() {
                return;
            }
        }
    }

    async fn connect_once(&self, tx: &mpsc::UnboundedSender<ClientEvent>) -> Result<()> {
        let mut session = BiliSession::new(self.cookies.clone())?;
        let mut connection = DanmakuConnection::connect(&mut session, self.room_id).await?;
        if tx
            .send(ClientEvent::Connected {
                room: connection.room.clone(),
            })
            .is_err()
        {
            return Ok(());
        }
        loop {
            match connection.next_event().await {
                Ok(Some(event)) => {
                    if tx.send(ClientEvent::Danmaku(event)).is_err() {
                        return Ok(());
                    }
                }
                Ok(None) => bail!("连接被服务端关闭"),
                Err(err) => return Err(err),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::DanmuHost;

    #[test]
    fn pick_host_prefers_wss() {
        let danmu = DanmuInfo {
            token: "t".into(),
            host_list: vec![
                DanmuHost {
                    host: "a".into(),
                    wss_port: 0,
                    ws_port: 2243,
                },
                DanmuHost {
                    host: "b".into(),
                    wss_port: 443,
                    ws_port: 2244,
                },
            ],
        };
        assert_eq!(pick_host(&danmu).unwrap().host, "b");
    }
}
