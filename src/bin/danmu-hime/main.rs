//! 挂在桌面角落的弹幕浮层：SCTK 开 wlr-layer-shell 面，wl_shm 共享内存 +
//! 纯软件渲染（tiny-skia 画底板、ab_glyph 刷字）。
//!
//! 没有弹幕时整层完全透明、什么都不画；有弹幕时是一块固定宽度的暗色半透明
//! 方形底板，高度刚好裹住当前几条，新的把旧的往上推，超过高度上限的不画，
//! 到 TTL 后连底板一起淡出。
//!
//! 不依赖 GTK/Qt/GPU，进程能保持在个位数 MB。

mod png;
mod render;

use anyhow::{Context, Result, bail};
use danmu_hime::{
    ClientEvent, Cookies, DanmakuClient, DanmakuEvent, load_cached_cookies, parse_room_arg,
};
use render::{DrawLine, Kind, Renderer, Theme};
use smithay_client_toolkit::reexports::calloop::{
    EventLoop, LoopHandle, RegistrationToken,
    channel::{Event as ChannelEvent, Sender},
    timer::{TimeoutAction, Timer},
};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
};
// 分数缩放：合成器（niri/Hyprland…）说 1.25 倍时，wl_surface 的整数 scale 不够用，
// 得靠 wp_fractional_scale_v1 问出比例、再用 wp_viewporter 把缓冲缩回逻辑尺寸。
use wayland_protocols::wp::{
    fractional_scale::v1::client::{
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_fractional_scale_v1::{self, WpFractionalScaleV1},
    },
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};

/// 最短重绘间隔：弹幕一瞬间来一串时别画好几次，等下一帧一起画。
const MIN_DRAW_INTERVAL: Duration = Duration::from_millis(8);

/// 推弹幕动画的时间常数（秒）：越小收得越快。
const SLIDE_TAU: f32 = 0.055;
/// 动画期间的定时器节奏（60fps）：合成器给的 frame 回调不一定按刷新率来
/// （niri 上是跟着我们自己的 commit 回，等于没有节拍），动画的帧就自己推。
const FRAME_TICK: Duration = Duration::from_millis(16);

/// 空转时最多睡这么久就醒一次（为的是接住新来的弹幕、按点开始淡出）。
const IDLE_TICK: Duration = Duration::from_secs(1);

struct Args {
    room: String,
    cookie: Option<String>,
    width: u32,
    height: u32,
    margin: i32,
    anchor: Anchor,
    theme: Theme,
    max_lines: usize,
    /// 一条弹幕在屏幕上待多久（秒），之后开始淡出。
    ttl: f32,
    /// 淡出用多久（秒）。
    fade: f32,
    font: Option<String>,
    /// 手动指定设备像素比；不给就跟合成器走（分数缩放也认）。
    scale: Option<f32>,
    /// 整体放大倍数：字和行距一起放大，跟设备像素比无关（默认 1.0）。
    zoom: f32,
    /// 挂到哪块输出（显示器）上，不给就由合成器决定。
    output: Option<String>,
    /// 实际生效的配置文件路径（没有就是 --no-config）。
    config_path: Option<PathBuf>,
    /// 彩色 emoji 字体；不给就 fc-match 找 emoji，找不到就跳过画不出来的字。
    emoji_font: Option<String>,
    /// 要不要画粉丝牌子（[牌子名·等级]）。
    medal: bool,
    /// 要不要画礼物（醒目留言一直画）。
    gift: bool,
    /// 位置微调：正数往右/往下推（在 九宫格 + margin 之上再挪）。
    offset_x: i32,
    offset_y: i32,
}

impl Args {
    /// 把当前生效的设置导成一份完整配置（`--print-config` 用）。
    fn to_file_config(&self) -> FileConfig {
        FileConfig {
            room: Some(self.room.clone()),
            cookie: self.cookie.clone(),
            output: self.output.clone(),
            anchor: Some(anchor_name(self.anchor).to_string()),
            width: Some(self.width),
            height: Some(self.height),
            margin: Some(self.margin),
            offset_x: Some(self.offset_x),
            offset_y: Some(self.offset_y),
            font_size: Some(self.theme.font_size),
            line_gap: Some(self.theme.line_gap),
            opacity: Some(self.theme.panel_alpha),
            ttl: Some(self.ttl),
            fade: Some(self.fade),
            max_lines: Some(self.max_lines),
            font: self.font.clone(),
            emoji_font: self.emoji_font.clone(),
            medal: Some(self.medal),
            gift: Some(self.gift),
            name_color: Some(format!(
                "#{:02x}{:02x}{:02x}",
                self.theme.name_color.0, self.theme.name_color.1, self.theme.name_color.2
            )),
            text_color: Some(format!(
                "#{:02x}{:02x}{:02x}",
                self.theme.text_color.0, self.theme.text_color.1, self.theme.text_color.2
            )),
            panel_color: Some(format!(
                "#{:02x}{:02x}{:02x}",
                self.theme.panel_rgb.0, self.theme.panel_rgb.1, self.theme.panel_rgb.2
            )),
            scale: self.scale,
            zoom: Some(self.zoom),
        }
    }
}

fn usage() -> &'static str {
    "用法: danmu-hime <房间号|直播间链接> [选项]

选项:
  --width <像素>        浮层宽（默认 381，暗色底板铺满这个宽度）
  --height <像素>       高度上限（默认 560），装不下的老弹幕就不画了
  --margin <像素>       距屏幕边缘（默认 20）
  --anchor <位置>       bottom-right|bottom-left|top-right|…（默认 bottom-right）
  --font-size <像素>    字号（默认 30，是 em 尺寸：汉字实际约占九成）
  --zoom <倍数>         字和行距一起放大（默认 1.0；跟 --scale 的设备像素比无关）
  --line-gap <像素>     两行之间的空隙（默认 4；底板上仍然是连着的）
  --opacity <0-1>       暗色底板的不透明度（默认 0.6，0 = 只有字没底）
  --ttl <秒>            一条弹幕待多久后开始淡出（默认 14）
  --fade <秒>           淡出用多久（默认 1.0）
  --max-lines <条数>    最多记多少条（默认 101）
  --font <路径>         字体文件，默认用 fontconfig 找中文字体
  --emoji-font <路径>   彩色 emoji 字体（默认 fc-match emoji，找不到就跳过 emoji）
  --scale <倍数>        设备像素比，默认跟随合成器（HiDPI/分数缩放）
  --output <名字>       挂到哪块显示器上，默认让合成器挑
  --cookie \"...\"       登录 cookie（默认读 bilibili_live_stream 脚本的缓存）

  --config <路径>       配置文件（默认 ~/.config/danmu-hime/config.json，
                        文件里的项都可以被命令行覆盖；改了这个文件浮层会热重载）
  --no-config           不读配置文件
  --print-config        把当前生效的配置以 JSON 打到 stdout 就退出"
}

/// 配置文件（`~/.config/danmu-hime/config.json`）的内容。
/// 每一项都可选：没写的用默认值，所以 GUI 只写自己关心的几项也没问题。
/// 数字项：381、381.0、"381" 都收——GUI 或者手写都可能给成浮点。
fn config_number<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Number {
        Int(i64),
        Float(f64),
        Text(String),
    }
    let value = Option::<Number>::deserialize(deserializer)?;
    Ok(match value {
        None => None,
        Some(Number::Int(number)) => Some(number as f64),
        Some(Number::Float(number)) => Some(number),
        Some(Number::Text(text)) => text.trim().parse().ok(),
    })
}

fn de_u32<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(config_number(deserializer)?.map(|number| number.round().clamp(0.0, u32::MAX as f64) as u32))
}

/// 浮点项也容忍 `1.5` / `2` / `"1.5"` 三种写法（GUI 有时候写字符串）。
fn de_f32_opt<'de, D>(deserializer: D) -> Result<Option<f32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(config_number(deserializer)?
        .map(|number| number.clamp(0.0, 1000.0) as f32))
}

fn de_i32<'de, D>(deserializer: D) -> Result<Option<i32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(config_number(deserializer)?.map(|number| number.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32))
}

fn de_usize<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(config_number(deserializer)?.map(|number| number.round().max(0.0) as usize))
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
struct FileConfig {
    room: Option<String>,
    cookie: Option<String>,
    output: Option<String>,
    anchor: Option<String>,
    #[serde(default, deserialize_with = "de_u32")]
    width: Option<u32>,
    #[serde(default, deserialize_with = "de_u32")]
    height: Option<u32>,
    #[serde(default, deserialize_with = "de_i32")]
    margin: Option<i32>,
    font_size: Option<f32>,
    line_gap: Option<f32>,
    opacity: Option<f32>,
    ttl: Option<f32>,
    fade: Option<f32>,
    #[serde(default, deserialize_with = "de_usize")]
    max_lines: Option<usize>,
    font: Option<String>,
    emoji_font: Option<String>,
    scale: Option<f32>,
    /// 整体放大倍数（1.0 = 原样）。
    #[serde(default, deserialize_with = "de_f32_opt")]
    zoom: Option<f32>,
    /// 要不要画粉丝牌子；不写就是画。
    medal: Option<bool>,
    /// 昵称 / 正文 / 底板的颜色，`#rrggbb`。
    name_color: Option<String>,
    text_color: Option<String>,
    panel_color: Option<String>,

    /// 要不要画礼物；不写就是画。
    gift: Option<bool>,
    /// 位置微调（像素，可负）。
    #[serde(default, deserialize_with = "de_i32")]
    offset_x: Option<i32>,
    #[serde(default, deserialize_with = "de_i32")]
    offset_y: Option<i32>,
}

/// 默认配置文件路径：`$XDG_CONFIG_HOME/danmu-hime/config.json`。
fn default_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("danmu-hime").join("config.json"))
}

impl FileConfig {
    /// 读一份配置文件；文件不存在就当空配置（不是错误）。
    fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("读不了 {}", path.display()))?;
        serde_json::from_slice(&bytes).with_context(|| format!("{} 不是合法 JSON", path.display()))
    }

    fn load_if_exists(path: &Path) -> Result<Self> {
        if path.exists() { Self::load(path) } else { Ok(Self::default()) }
    }

    /// 把配置里的项盖到参数上（命令行随后还会覆盖一遍，所以命令行优先）。
    fn apply_to(&self, args: &mut Args) -> Result<()> {
        if let Some(room) = &self.room {
            args.room = room.clone();
        }
        if let Some(cookie) = &self.cookie {
            args.cookie = Some(cookie.clone());
        }
        if let Some(output) = &self.output {
            args.output = Some(output.clone());
        }
        if let Some(zoom) = self.zoom {
            args.zoom = zoom.clamp(0.2, 5.0);
        }
        if let Some(anchor) = &self.anchor {
            args.anchor = parse_anchor(anchor)?;
        }
        if let Some(width) = self.width {
            args.width = width;
        }
        if let Some(height) = self.height {
            args.height = height;
        }
        if let Some(margin) = self.margin {
            args.margin = margin;
        }
        if let Some(size) = self.font_size {
            args.theme.font_size = size;
        }
        if let Some(gap) = self.line_gap {
            args.theme.line_gap = gap;
        }
        if let Some(opacity) = self.opacity {
            args.theme.panel_alpha = opacity;
        }
        if let Some(ttl) = self.ttl {
            args.ttl = ttl;
        }
        if let Some(fade) = self.fade {
            args.fade = fade;
        }
        if let Some(max_lines) = self.max_lines {
            args.max_lines = max_lines;
        }
        if let Some(font) = &self.font {
            args.font = Some(font.clone());
        }
        if let Some(x) = self.offset_x {
            args.offset_x = x;
        }
        if let Some(y) = self.offset_y {
            args.offset_y = y;
        }
        if let Some(gift) = self.gift {
            args.gift = gift;
        }
        if let Some(color) = &self.name_color {
            args.theme.name_color = parse_color(color, "name_color")?;
        }
        if let Some(color) = &self.text_color {
            args.theme.text_color = parse_color(color, "text_color")?;
        }
        if let Some(color) = &self.panel_color {
            args.theme.panel_rgb = parse_color(color, "panel_color")?;
        }
        if let Some(medal) = self.medal {
            args.medal = medal;
        }
        if let Some(font) = &self.emoji_font {
            args.emoji_font = Some(font.clone());
        }
        if let Some(scale) = self.scale {
            args.scale = Some(scale);
        }
        Ok(())
    }
}

impl Default for Args {
    fn default() -> Self {
        Self {
            room: String::new(),
            cookie: None,
            width: 381,
            height: 560,
            margin: 20,
            offset_x: -20,
            offset_y: 456,
            anchor: Anchor::BOTTOM | Anchor::RIGHT,
            theme: Theme::default(),
            max_lines: 101,
            ttl: 14.0,
            fade: 1.0,
            font: None,
            scale: None,
            zoom: 1.0,
            output: None,
            config_path: None,
            emoji_font: None,
            medal: false,
            gift: true,
        }
    }
}

fn parse_args() -> Result<Args> {
    let mut args = Args::default();
    // 配置文件的路径要先扫一遍（这样 --config 也能写在后面）
    let mut config_path = default_config_path();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut peek = argv.iter();
    while let Some(arg) = peek.next() {
        match arg.as_str() {
            "--config" => config_path = peek.next().map(PathBuf::from),
            "--no-config" => config_path = None,
            _ => {}
        }
    }
    if let Some(path) = &config_path {
        // 配置坏了也别把浮层拦在门外：警告一声，按命令行/默认值继续
        match FileConfig::load_if_exists(path) {
            Ok(config) => config.apply_to(&mut args)?,
            Err(err) => {
                eprintln!("# 配置文件读不了，这次先按命令行/默认值来：{err}");
                eprintln!("# 改好之后不用重启，浮层每次改动都会重新读");
            }
        }
    }
    args.config_path = config_path.clone();
    // 命令行在这次解析里覆盖配置文件（后写的赢）
    let mut room_from_cli = false;
    let mut argv = argv.into_iter();
    while let Some(arg) = argv.next() {
        let mut value = |flag: &str| -> Result<String> {
            argv.next().with_context(|| format!("{flag} 后面要跟一个值"))
        };
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            "--width" => args.width = value("--width")?.parse().context("--width 要数字")?,
            "--height" => args.height = value("--height")?.parse().context("--height 要数字")?,
            "--margin" => args.margin = value("--margin")?.parse().context("--margin 要数字")?,
            "--anchor" => args.anchor = parse_anchor(&value("--anchor")?)?,
            "--font-size" => {
                args.theme.font_size = value("--font-size")?.parse().context("--font-size 要数字")?
            }
            "--opacity" => {
                args.theme.panel_alpha = value("--opacity")?.parse().context("--opacity 要数字")?
            }
            "--line-gap" => {
                args.theme.line_gap = value("--line-gap")?.parse().context("--line-gap 要数字")?
            }
            "--config" => {
                let _ = value("--config")?;
            }
            "--no-config" => {}
            "--print-config" => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&args.to_file_config())
                        .context("序列化配置失败")?
                );
                std::process::exit(0);
            }
            "--ttl" => {
                let ttl: f32 = value("--ttl")?.parse().context("--ttl 要数字")?;
                if ttl <= 0.0 {
                    bail!("--ttl 要是正数（秒）");
                }
                args.ttl = ttl;
            }
            "--fade" => {
                let fade: f32 = value("--fade")?.parse().context("--fade 要数字")?;
                if fade <= 0.0 {
                    bail!("--fade 要是正数（秒）");
                }
                args.fade = fade;
            }
            "--max-lines" => {
                args.max_lines = value("--max-lines")?.parse().context("--max-lines 要数字")?
            }
            "--font" => args.font = Some(value("--font")?),
            "--no-gift" => args.gift = false,
            "--offset-x" => args.offset_x = value("--offset-x")?.parse().context("--offset-x 要整数")?,
            "--offset-y" => args.offset_y = value("--offset-y")?.parse().context("--offset-y 要整数")?,
            "--gift" => args.gift = true,
            "--name-color" => {
                args.theme.name_color = parse_color(&value("--name-color")?, "--name-color")?
            }
            "--text-color" => {
                args.theme.text_color = parse_color(&value("--text-color")?, "--text-color")?
            }
            "--panel-color" => {
                args.theme.panel_rgb = parse_color(&value("--panel-color")?, "--panel-color")?
            }
            "--emoji-font" => args.emoji_font = Some(value("--emoji-font")?),
            "--scale" => {
                let scale: f32 = value("--scale")?.parse().context("--scale 要数字")?;
                if !(1.0..=8.0).contains(&scale) {
                    bail!("--scale 要在 1.0 到 8.0 之间");
                }
                args.scale = Some(scale);
            }
            "--zoom" => {
                let zoom: f32 = value("--zoom")?.parse().context("--zoom 要数字")?;
                if !(0.2..=5.0).contains(&zoom) {
                    bail!("--zoom 要在 0.2 到 5.0 之间");
                }
                args.zoom = zoom;
            }
            "--output" => args.output = Some(value("--output")?),
            "--cookie" => args.cookie = Some(value("--cookie")?),
            other if other.starts_with('-') => bail!("不认识的参数 {other}\n{}", usage()),
            other => {
                // 配置文件里的房间号可以被命令行覆盖，同一条命令行上给两次才算错
                if room_from_cli {
                    bail!("房间号只能给一个\n{}", usage());
                }
                args.room = other.to_string();
                room_from_cli = true;
            }
        }
    }
    if args.room.is_empty() {
        bail!("{}", usage());
    }
    if args.width < 160 || args.height < 120 {
        bail!("浮层太小了，至少 160x120");
    }
    Ok(args)
}

fn anchor_name(anchor: Anchor) -> &'static str {
    match (
        anchor.contains(Anchor::TOP),
        anchor.contains(Anchor::BOTTOM),
        anchor.contains(Anchor::LEFT),
        anchor.contains(Anchor::RIGHT),
    ) {
        (true, _, true, _) => "top-left",
        (true, _, _, true) => "top-right",
        (_, true, true, _) => "bottom-left",
        (_, true, _, true) => "bottom-right",
        (true, _, _, _) => "top",
        (_, true, _, _) => "bottom",
        (_, _, true, _) => "left",
        (_, _, _, true) => "right",
        _ => "center",
    }
}


/// 解析 `#7aa2f7` / `7aa2f7` 这种十六进制颜色。
fn parse_color(value: &str, flag: &str) -> Result<(u8, u8, u8)> {
    let text = value.trim().trim_start_matches('#');
    if text.len() != 6 {
        bail!("{flag} 要 #rrggbb 这种颜色");
    }
    let number = u32::from_str_radix(text, 16).with_context(|| format!("{flag} 要 #rrggbb 这种颜色"))?;
    Ok((((number >> 16) & 0xff) as u8, ((number >> 8) & 0xff) as u8, (number & 0xff) as u8))
}

fn parse_anchor(value: &str) -> Result<Anchor> {
    Ok(match value {
        "top-left" => Anchor::TOP | Anchor::LEFT,
        "top-right" => Anchor::TOP | Anchor::RIGHT,
        "bottom-left" => Anchor::BOTTOM | Anchor::LEFT,
        "bottom-right" => Anchor::BOTTOM | Anchor::RIGHT,
        "top" | "top-center" => Anchor::TOP,
        "bottom" | "bottom-center" => Anchor::BOTTOM,
        "left" => Anchor::LEFT,
        "right" => Anchor::RIGHT,
        "center" => Anchor::empty(),
        other => bail!("不认识的锚点 {other}（top-right / bottom-right / …）"),
    })
}


fn main() -> Result<()> {
    let args = parse_args()?;
    let room_id = parse_room_arg(&args.room)?;
    let cookies = match &args.cookie {
        Some(raw) => Cookies::from_cookie_string(raw),
        None => load_cached_cookies(),
    };
    let (font_path, font_index) = find_font(args.font.as_deref())?;
    let font_file = std::fs::File::open(&font_path)
        .with_context(|| format!("打不开字体 {}", font_path.display()))?;
    // SAFETY: 字体文件在进程生命周期里不会被改（系统字体）。
    let font_map = unsafe { memmap2::Mmap::map(&font_file) }
        .with_context(|| format!("mmap 字体失败 {}", font_path.display()))?;
    // 随机读字体里的表，别让内核预读把 19MB 整块摸进来（实测能省 8MB 常驻）。
    font_map.advise(memmap2::Advice::Random).ok();

    let conn = Connection::connect_to_env()
        .context("连不上 Wayland 合成器（检查 WAYLAND_DISPLAY / XDG_RUNTIME_DIR）")?;
    let (globals, mut event_queue) =
        registry_queue_init(&conn).context("枚举 Wayland 全局对象失败")?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor 不可用")?;
    let layer_shell =
        LayerShell::bind(&globals, &qh).context("这个合成器不支持 wlr-layer-shell")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm 不可用")?;

    let surface = compositor.create_surface(&qh);
    // 空输入区：光设 keyboard_interactivity=None 只管键盘，指针照样被整块浮层吃掉；
    // 挂个空的输入区域，点击才穿得到下面的窗口。
    let input_region = Region::new(&compositor).context("创建输入区域失败")?;
    let output_state = OutputState::new(&globals, &qh);
    // 分数缩放是可选的：合成器不支持就退回 wl_surface 的整数 scale。
    let fractional = globals
        .bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ())
        .ok();
    let viewporter = globals.bind::<WpViewporter, _, _>(&qh, 1..=1, ()).ok();
    let fractional_scale = fractional
        .as_ref()
        .map(|manager| manager.get_fractional_scale(&surface, &qh, ()));
    let viewport = viewporter
        .as_ref()
        .map(|viewporter| viewporter.get_viewport(&surface, &qh, ()));

    let pool = SlotPool::new((args.width * args.height * 4) as usize, &shm)
        .context("创建 shm 内存池失败")?;

    SHOW_MEDAL.store(args.medal, std::sync::atomic::Ordering::Relaxed);
    SHOW_GIFT.store(args.gift, std::sync::atomic::Ordering::Relaxed);
    let restart_only = RestartOnly {
        room: args.room.clone(),
        cookie: args.cookie.clone(),
        output: args.output.clone(),
        font: args.font.clone(),
        emoji_font: args.emoji_font.clone(),
    };
    // 测试弹幕文件固定放在配置目录里（GUI 的「测试弹幕」按钮就写这儿）；
    // 就算浮层是 --no-config 跑的，也照样能收到测试弹幕。
    let test_path = args
        .config_path
        .clone()
        .or_else(default_config_path)
        .map(|path| path.with_file_name("test-danmaku.jsonl"));
    // 启动时跳过文件里已有的内容：那是上次运行留下的，别再重播一遍。
    let test_offset = test_path
        .as_ref()
        .and_then(|path| std::fs::metadata(path).ok())
        .map_or(0, |meta| meta.len());

    let mut state = Overlay {
        registry_state: RegistryState::new(&globals),
        output_state,
        shm,
        pool,
        layer_shell,
        surface,
        input_region,
        layer: None,
        // 永远用 overlay 层：顶层(top)会被全屏窗口盖住，改层级得重建 surface，
        // 做成配置项没意义（改了不起作用），索性写死。
        layer_kind: Layer::Overlay,
        anchor: args.anchor,
        margin: args.margin,
        ui_tx: None,
        test_path,
        test_offset,
        offset_x: args.offset_x,
        offset_y: args.offset_y,
        target_output: args.output.clone(),
        width: args.width,
        height: args.height,
        buffer_scale: 1,
        fractional_scale,
        viewport,
        fractional120: 120,
        forced_scale: args.scale,
        exit: false,
        dirty: false,
        slide: 0.0,
        last_tick: Instant::now(),
        timer_fast: false,
        loop_handle: None,
        timer_key: None,
        last_draw: Instant::now()
            .checked_sub(MIN_DRAW_INTERVAL)
            .unwrap_or_else(Instant::now),
        entries: VecDeque::new(),
        max_lines: args.max_lines,
        ttl: Duration::from_secs_f32(args.ttl),
        fade: Duration::from_secs_f32(args.fade),
        renderer: Renderer::new(font_map, font_index, args.theme),
        config_path: args.config_path.clone(),
        config_stamp: args.config_path
            .as_ref()
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|meta| meta.modified().ok()),
        restart_only,
    };
    state.renderer.set_zoom(args.zoom);

    // 彩色 emoji 字体是可选的：挂上之后画不出来的字符就去它那儿找位图。
    match find_emoji_font(args.emoji_font.as_deref()) {
        Some((path, index)) => match std::fs::File::open(&path) {
            Ok(file) => match unsafe { memmap2::Mmap::map(&file) } {
                Ok(map) => {
                    map.advise(memmap2::Advice::Random).ok();
                    state.renderer.set_emoji_font(map, index);
                    eprintln!("# 彩色 emoji 字体: {}", path.display());
                }
                Err(err) => eprintln!("# emoji 字体 mmap 失败（emoji 会跳过）: {err}"),
            },
            Err(err) => eprintln!("# 打不开 emoji 字体 {}（emoji 会跳过）: {err}", path.display()),
        },
        None => eprintln!("# 没找到彩色 emoji 字体，emoji 会跳过（--emoji-font 可以指定）"),
    }

    // 显示器的名字要等一轮事件才从 wl_output 里收到；收到再建浮层，
    // 才能按 `--output` 挂到指定那块屏上（不然只能由合成器挑）。
    event_queue
        .roundtrip(&mut state)
        .context("同步 Wayland 状态失败")?;
    if let Some(name) = &args.output
        && state.output_by_name(name).is_none()
    {
        bail!("找不到叫 {name} 的显示器（niri msg outputs 能列出名字）");
    }
    state.create_layer(&qh);

    let mut event_loop: EventLoop<Overlay> = EventLoop::try_new().context("创建事件循环失败")?;
    let loop_handle = event_loop.handle();

    let (ui_tx, ui_rx) = smithay_client_toolkit::reexports::calloop::channel::channel::<UiEvent>();
    state.ui_tx = Some(ui_tx.clone());
    loop_handle
        .insert_source(ui_rx, |event, _, state: &mut Overlay| {
            if let ChannelEvent::Msg(message) = event {
                state.on_ui_event(message);
            }
        })
        .map_err(|err| anyhow::anyhow!("注册弹幕事件源失败: {err}"))?;

    // 定时器：没动画的时候它只负责「到点了开始淡出」「把过期的清掉」，
    // 最多 1 秒醒一次；动画一生效（tick 里改 timer_fast）就换成按帧的节奏。
    let timer_key = loop_handle
        .insert_source(
            Timer::from_duration(IDLE_TICK),
            |_, _, state: &mut Overlay| state.tick(),
        )
        .map_err(|err| anyhow::anyhow!("注册定时器失败: {err}"))?;
    state.loop_handle = Some(loop_handle.clone());
    state.timer_key = Some(timer_key);

    spawn_danmaku(room_id, cookies, ui_tx);

    WaylandSource::new(conn, event_queue)
        .insert(loop_handle)
        .map_err(|err| anyhow::anyhow!("注册 Wayland 事件源失败: {err}"))?;

    let draw_handle = qh.clone();
    event_loop
        .run(None, &mut state, move |state| {
            state.redraw_if_dirty(&draw_handle);
        })
        .context("事件循环退出")?;
    Ok(())
}

enum UiEvent {
    /// 一条弹幕，拆成「前缀（昵称/粉丝牌）」和正文。
    Danmaku { prefix: String, text: String },
    /// 连不上之类的问题，也用一条会淡出的提示表示一下。
    Notice(String),
    /// 后台下好的表情原图（主线程负责交给渲染器）。
    Emote {
        ch: char,
        bytes: std::sync::Arc<Vec<u8>>,
    },
}

/// 屏上的一条弹幕（画成什么样由渲染层决定）。
struct Entry {
    kind: Kind,
    prefix: String,
    text: String,
    received: Instant,
}

struct Overlay {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer_shell: LayerShell,
    /// 留着 wl_surface 的引用：layer surface 万一要重建还用得上。
    surface: wl_surface::WlSurface,
    /// 空白输入区域（内容为空），让指针事件穿过去。
    input_region: Region,
    layer: Option<LayerSurface>,
    layer_kind: Layer,
    anchor: Anchor,
    margin: i32,
    /// `--output` 指定的显示器名字。
    target_output: Option<String>,
    width: u32,
    height: u32,
    /// 合成器要的设备像素比（HiDPI / 分数缩放时 >1）。
    buffer_scale: i32,
    /// wp_fractional_scale_v1 对象：合成器给的分数比例从它的事件里来。
    /// 只是把它拿在手里不让对象销毁（销毁了就没事件了）。
        /// 只用来保活这个对象（分数缩放对象销毁了缩放就不生效了）
    #[allow(dead_code)]
    fractional_scale: Option<WpFractionalScaleV1>,
    /// wp_viewport：把设备像素的缓冲映射回逻辑尺寸。
    viewport: Option<WpViewport>,
    /// 分数缩放，单位 1/120（120 = 1.0，150 = 1.25）。
    fractional120: u32,
    /// `--scale` 强制的像素比。
    forced_scale: Option<f32>,
    exit: bool,
    dirty: bool,
    /// 推弹幕的动画位移（设备像素）：新弹幕进来时加上一行的高度，
    /// 之后每帧指数衰减回 0，看上去就是整摆被顶上去。
    slide: f32,
    /// 上一次推进动画的时刻（算 dt 用）。
    last_tick: Instant,
    /// 定时器现在是不是「按帧刷」的快节奏。
    timer_fast: bool,
    /// 事件循环的 handle 和定时器的登记号：需要把定时器提前时用它换掉。
    loop_handle: Option<LoopHandle<'static, Overlay>>,
    timer_key: Option<RegistrationToken>,
    last_draw: Instant,
    /// 屏上的弹幕，老的在前（渲染时新的贴底）。
    entries: VecDeque<Entry>,
    max_lines: usize,
    /// 存活时长：超过就开始淡出。
    ttl: Duration,
    /// 淡出时长。
    fade: Duration,
    renderer: Renderer,
    /// 配置文件路径（`--config` / 默认路径），用来做热重载。
    config_path: Option<PathBuf>,
    /// 上次看到的配置文件修改时间。
    config_stamp: Option<SystemTime>,
    /// 改了必须重启才生效的那几项（热重载时用来提醒）。
    restart_only: RestartOnly,
    /// 位置微调（像素，可负）。
    offset_x: i32,
    offset_y: i32,
    /// UI 事件通道的发送端（测试弹幕也走这条道，跟真弹幕一模一样）。
    ui_tx: Option<smithay_client_toolkit::reexports::calloop::channel::Sender<UiEvent>>,
    /// GUI 的「测试弹幕」按钮往这里追加行，就放在配置文件旁边。
    test_path: Option<PathBuf>,
    test_offset: u64,

}

/// 热重载管不了的设置：房间、cookie、显示器、字体（字体要重新 mmap）。
#[derive(Clone, PartialEq)]
struct RestartOnly {
    room: String,
    cookie: Option<String>,
    output: Option<String>,
    font: Option<String>,
    emoji_font: Option<String>,
}

impl Overlay {
    fn output_by_name(&self, name: &str) -> Option<wl_output::WlOutput> {
        self.output_state.outputs().find(|output| {
            self.output_state
                .info(output)
                .and_then(|info| info.name)
                .is_some_and(|actual| actual == name)
        })
    }

    /// 建 layer surface（`main` 里先 roundtrip 一轮拿到显示器名字再调）。
    fn create_layer(&mut self, qh: &QueueHandle<Self>) {
        let output = self
            .target_output
            .as_ref()
            .and_then(|name| self.output_by_name(name));
        let layer = self.layer_shell.create_layer_surface(
            qh,
            self.surface.clone(),
            self.layer_kind,
            Some("danmu-hime"),
            output.as_ref(),
        );
        layer.set_anchor(self.anchor);
        layer.set_size(self.width, self.height);
        let (top, right, bottom, left) = self.margins();
        layer.set_margin(top, right, bottom, left);
        // 不抢键盘，鼠标也点穿过去，就是个挂件。
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(0);
        // 输入区一直是空的话，点击会直接落到下面的窗口；
        // 这是 wl_surface 的持久状态，设一次就行。
        layer
            .wl_surface()
            .set_input_region(Some(self.input_region.wl_region()));
        layer.commit();
        self.layer = Some(layer);
    }

    fn on_ui_event(&mut self, message: UiEvent) {
        let (kind, prefix, text) = match message {
            UiEvent::Danmaku { prefix, text } => (Kind::Danmaku, prefix, text),
            UiEvent::Notice(text) => (Kind::System, String::new(), text),
            UiEvent::Emote { ch, bytes } => {
                // 表情原图到了：挂到渲染器上，之后这个字符一律贴真图
                if self.renderer.load_emote(ch, &bytes) {
                    self.dirty = true;
                }
                return;
            }
        };
        // 文本里出现内置表情（本地测试弹幕也算）就先把它下下来——弹幕自带
        // url 的情况在弹幕线程里已经排过队了，这里主要照顾没有 url 的场景。
        if kind == Kind::Danmaku
            && let Some(tx) = self.ui_tx.clone()
        {
            for (token, url) in seed_emotes_in(&text) {
                ensure_emote(&tx, emote_placeholder(&token), &url);
            }
        }
        self.entries.push_back(Entry {
            kind,
            prefix,
            text,
            received: Instant::now(),
        });
        while self.entries.len() > self.max_lines {
            self.entries.pop_front();
        }
        // 注意：这里不能整块往下压一行再滑回去——那样屏幕上已有的弹幕
        // （连正在淡出的）都会跟着闪一下。改成只给新来的那条做入场动画，
        // 见 DrawLine::enter / render.rs 里那个 shift。
        self.slide = 0.0;
        self.last_tick = Instant::now();
        // 定时器可能正睡在 1 秒后的空转节奏上，赶紧把它提前，动画才接得上。
        self.hurry_timer();
        self.dirty = true;
    }

    /// 让定时器尽快再醒一次（动画中改成按帧的节奏）。
    fn hurry_timer(&mut self) {
        if !self.timer_fast {
            self.timer_fast = true;
            self.reschedule_timer(FRAME_TICK);
        }
    }

    /// 换掉定时器，让它在 `delay` 之后响（remove + insert，calloop 0.14 只能这么改）。
    fn reschedule_timer(&mut self, delay: Duration) {
        let (Some(handle), Some(key)) = (self.loop_handle.clone(), self.timer_key.take()) else {
            return;
        };
        handle.remove(key);
        match handle.insert_source(
            Timer::from_duration(delay),
            |_, _, state: &mut Overlay| state.tick(),
        ) {
            Ok(token) => self.timer_key = Some(token),
            Err(err) => eprintln!("# 重排定时器失败: {err}"),
        }
    }

    /// 配置文件被改了吗？改了就把「显示类」的设置热更上去（房间之类的只能重启）。
    fn reload_config_if_changed(&mut self) {
        let Some(path) = self.config_path.clone() else {
            return;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        let stamp = meta.modified().ok();
        if stamp.is_none() || stamp == self.config_stamp {
            return;
        }
        self.config_stamp = stamp;
        let config = match FileConfig::load(&path) {
            Ok(config) => config,
            Err(err) => {
                eprintln!("# 配置文件读不动，先不管它: {err}");
                return;
            }
        };

        // 只能重启才生效的：跟当前值比一比，变了就提醒一句
        let next = RestartOnly {
            room: config.room.clone().unwrap_or_else(|| self.restart_only.room.clone()),
            cookie: config.cookie.clone().or_else(|| self.restart_only.cookie.clone()),
            output: config.output.clone().or_else(|| self.restart_only.output.clone()),
            font: config.font.clone().or_else(|| self.restart_only.font.clone()),
            emoji_font: config
                .emoji_font
                .clone()
                .or_else(|| self.restart_only.emoji_font.clone()),
        };
        if next != self.restart_only {
            let mut what = Vec::new();
            if next.room != self.restart_only.room {
                what.push("房间");
            }
            if next.cookie != self.restart_only.cookie {
                what.push("cookie");
            }
            if next.output != self.restart_only.output {
                what.push("显示器");
            }
            if next.font != self.restart_only.font || next.emoji_font != self.restart_only.emoji_font {
                what.push("字体");
            }
            eprintln!("# 配置里改了 {}，重启浮层才生效", what.join("、"));
        }

        // 显示类的当场生效
        if let Some(size) = config.font_size {
            self.renderer.set_base_font_size(size);
        }
        if let Some(gap) = config.line_gap {
            self.renderer.set_base_line_gap(gap);
        }
        if let Some(zoom) = config.zoom {
            self.renderer.set_zoom(zoom);
        }
        if let Some(opacity) = config.opacity {
            self.renderer.theme.panel_alpha = opacity.clamp(0.0, 1.0);
        }
        if let Some(color) = config.name_color.as_deref().and_then(|c| parse_color(c, "name_color").ok()) {
            self.renderer.theme.name_color = color;
        }
        if let Some(color) = config.text_color.as_deref().and_then(|c| parse_color(c, "text_color").ok()) {
            self.renderer.theme.text_color = color;
        }
        if let Some(color) = config.panel_color.as_deref().and_then(|c| parse_color(c, "panel_color").ok()) {
            self.renderer.theme.panel_rgb = color;
        }
        if let Some(ttl) = config.ttl
            && ttl > 0.0
        {
            self.ttl = Duration::from_secs_f32(ttl);
        }
        if let Some(fade) = config.fade
            && fade > 0.0
        {
            self.fade = Duration::from_secs_f32(fade);
        }
        if let Some(max_lines) = config.max_lines {
            self.max_lines = max_lines.max(1);
        }
        if let Some(scale) = config.scale
            && (1.0..=8.0).contains(&scale)
        {
            self.forced_scale = Some(scale);
        }
        if let Some(answer) = config.anchor.as_deref().and_then(|v| parse_anchor(v).ok()) {
            self.anchor = answer;
        }
        if let Some(width) = config.width {
            self.width = width.max(160);
        }
        if let Some(height) = config.height {
            self.height = height.max(120);
        }
        if let Some(margin) = config.margin {
            self.margin = margin;
        }
        if let Some(medal) = config.medal {
            SHOW_MEDAL.store(medal, Ordering::Relaxed);
        }
        if let Some(gift) = config.gift {
            SHOW_GIFT.store(gift, Ordering::Relaxed);
        }
        if let Some(x) = config.offset_x {
            self.offset_x = x;
        }
        if let Some(y) = config.offset_y {
            self.offset_y = y;
        }
        if let Some(layer) = &self.layer {
            // 这些是 layer surface 的属性，改完要重新 commit 才生效
            layer.set_anchor(self.anchor);
            layer.set_size(self.width, self.height);
            let (top, right, bottom, left) = self.margins();
            layer.set_margin(top, right, bottom, left);
            layer.set_layer(self.layer_kind);
            layer.commit();
        }
        eprintln!("# 配置已热重载：显示类设置立即生效（{}x{}）", self.width, self.height);
        self.dirty = true;
    }

    /// 推进推弹幕动画：指数衰减，越接近 0 越慢，收尾不抖。
    /// 贴边时的四个边距：`margin` 加上用户微调（正数 = 往右下推）。
    fn margins(&self) -> (i32, i32, i32, i32) {
        margins_for(self.anchor, self.margin, self.offset_x, self.offset_y)
    }

    /// 读「测试弹幕」文件的新增行，当成真弹幕丢进同一条通道。
    fn poll_test_danmaku(&mut self) {
        let Some(path) = self.test_path.clone() else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        for line in take_new_lines(&mut self.test_offset, &text) {
            let Some(tx) = self.ui_tx.clone() else {
                return;
            };
            let _ = tx.send(UiEvent::Danmaku {
                prefix: "弹幕姬报告: ".to_string(),
                text: line,
            });
        }
    }

    fn advance(&mut self, now: Instant) {
        let dt = now
            .saturating_duration_since(self.last_tick)
            .as_secs_f32()
            .min(0.1);
        self.last_tick = now;
        if self.slide <= 0.0 {
            return;
        }
        self.slide *= (-dt / SLIDE_TAU).exp();
        if self.slide < 0.5 {
            self.slide = 0.0;
        }
        self.dirty = true;
    }

    /// 现在还有没有动画要放（推弹幕、或某条正在淡出）。
    fn needs_animation(&self, now: Instant) -> bool {
        self.slide > 0.0
            || self.entries.iter().any(|entry| {
                fading(now.saturating_duration_since(entry.received), self.ttl)
            })
    }

    /// 定时器醒了：清掉过期的、该刷的标记 dirty，并排下一次要醒的时刻。
    fn tick(&mut self) -> TimeoutAction {
        let now = Instant::now();
        self.reload_config_if_changed();
        self.poll_test_danmaku();
        self.advance(now);
        let before = self.entries.len();
        // 生命 = 抖动后的停留 + 淡出 + 收拢动画
        // 抖动最多 +15%，这里按最长的那条算，免得它还没收拢就被摘掉
        let life = self.ttl.mul_f32(1.15) + self.fade + COLLAPSE;
        self.entries
            .retain(|entry| now.saturating_duration_since(entry.received) < life);
        if self.entries.len() != before {
            self.dirty = true;
        }
        // 有动画要放（推弹幕/淡出）就按帧的节奏醒，安定了就回到「到点才醒」。
        self.timer_fast = self.needs_animation(now);
        if self.timer_fast {
            self.dirty = true;
            return TimeoutAction::ToDuration(FRAME_TICK);
        }
        TimeoutAction::ToDuration(self.next_wakeup(now))
    }

    /// 下一次该醒来的间隔：某条该开始淡出了、该清掉了，或者最多 1 秒醒一次看看。
    /// 动画帧不归它管（那是 frame 回调的事）。
    fn next_wakeup(&self, now: Instant) -> Duration {
        let mut delay = IDLE_TICK;
        for entry in &self.entries {
            delay = delay.min(wakeup_for(
                now.saturating_duration_since(entry.received),
                self.ttl,
                self.fade,
            ));
        }
        delay
    }

    /// 现在该画哪些弹幕：算好每条的透明度（淡出），并按高度上限截断。
    /// 长弹幕会换行成好几行，所以是从最新的往前按**视觉行数**凑，而不是按条数。
    fn visible_lines(&self, now: Instant, width: u32, capacity: usize) -> Vec<DrawLine> {
        let mut budget = capacity;
        let mut picked: Vec<&Entry> = Vec::new();
        for entry in self.entries.iter().rev() {
            let used = self.renderer.rows_of(width, &entry.prefix, &entry.text);
            if used > budget && !picked.is_empty() {
                break;
            }
            budget = budget.saturating_sub(used);
            picked.push(entry);
        }
        picked.reverse();
        picked
            .into_iter()
            .filter_map(|entry| {
                let alpha = self.alpha_of(entry, now);
                // 淡完之后还要留一会儿做「收拢」动画：不然后面那些行会一帧跳一格
                (alpha > 0.01 || self.collapse_of(entry, now) > 0.0).then(|| DrawLine {
                    kind: entry.kind,
                    prefix: entry.prefix.clone(),
                    text: entry.text.clone(),
                    alpha,
                    // 刚来的那条从下面滑上来（跟淡入同一段时间）
                    enter: fade_in_alpha(now.saturating_duration_since(entry.received)),
                    collapse: self.collapse_of(entry, now),
                })
            })
            .collect()
    }

    /// 每条弹幕的停留时间稍微抖一下（±15%），同一批就不会挤在同一帧集体消失。
    fn ttl_of(&self, entry: &Entry) -> Duration {
        let hash: u32 = entry
            .text
            .bytes()
            .fold(7u32, |acc, byte| acc.wrapping_mul(31).wrapping_add(byte as u32));
        let factor = 0.85 + 0.0003 * (hash % 1000) as f32;
        self.ttl.mul_f32(factor)
    }

    fn alpha_of(&self, entry: &Entry, now: Instant) -> f32 {
        let age = now.saturating_duration_since(entry.received);
        fade_alpha(age, self.ttl_of(entry), self.fade).min(fade_in_alpha(age))
    }

    /// 0..1：淡完之后再用 200ms 把这一条占的高度收掉，上面的行是滑下去而不是跳。
    fn collapse_of(&self, entry: &Entry, now: Instant) -> f32 {
        let age = now.saturating_duration_since(entry.received);
        let settle = self.ttl_of(entry) + self.fade;
        if age <= settle {
            return 1.0;
        }
        let t = (age - settle).as_secs_f32() / COLLAPSE.as_secs_f32();
        1.0 - (3.0 * t * t - 2.0 * t * t * t).clamp(0.0, 1.0)
    }

    fn redraw_if_dirty(&mut self, qh: &QueueHandle<Self>) {
        if self.dirty && self.last_draw.elapsed() >= MIN_DRAW_INTERVAL {
            self.draw(qh);
        }
    }

    /// 实际渲染用的设备像素比：`--scale` > 分数缩放 > 整数 scale。
    fn device_scale(&self) -> f32 {
        if let Some(scale) = self.forced_scale {
            return scale;
        }
        if self.viewport.is_some() {
            self.fractional120 as f32 / 120.0
        } else {
            self.buffer_scale.max(1) as f32
        }
    }

    fn draw(&mut self, _qh: &QueueHandle<Self>) {
        // 逻辑尺寸是 configure 给的；缓冲按设备像素比放大，字才不糊。
        let scale = self.device_scale();
        let (width, height) = (
            (self.width as f32 * scale).round().max(1.0) as u32,
            (self.height as f32 * scale).round().max(1.0) as u32,
        );
        self.renderer.set_scale(scale);
        let Some(layer) = &self.layer else { return };
        let surface = layer.wl_surface();
        match &self.viewport {
            // 有 viewporter：缓冲就是设备像素，逻辑尺寸靠 destination 缩回去，
            // 1.25 这种比例也能一个像素不糊（整数 scale 做不到）。
            Some(viewport) => {
                surface.set_buffer_scale(1);
                viewport.set_destination(self.width as i32, self.height as i32);
            }
            None => surface.set_buffer_scale(scale.round().max(1.0) as i32),
        }
        if std::env::var_os("DANMAKU_DEBUG").is_some() {
            eprintln!(
                "# t={:>8.1}ms 画 {}x{} 逻辑 → {}x{} 像素（scale={scale}, scroll={:.1}）",
                self.last_draw.elapsed().as_secs_f64() * 1000.0,
                self.width,
                self.height,
                width,
                height,
                self.slide,
            );
        }
        let stride = (width * 4) as i32;
        // 容量按设备像素高度算（逻辑高度 × 缩放才是缓冲的真实高度）
        let capacity = self.renderer.capacity(height);
        let lines = self.visible_lines(Instant::now(), width, capacity);
        let pixels = self.renderer.render(width, height, &lines, self.slide);

        let (buffer, canvas) = match self.pool.create_buffer(
            width as i32,
            height as i32,
            stride,
            wl_shm::Format::Argb8888,
        ) {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("# 创建共享内存缓冲失败: {err}");
                self.dirty = false;
                return;
            }
        };
        let length = pixels.len().min(canvas.len());
        canvas[..length].copy_from_slice(&pixels[..length]);

        surface.damage_buffer(0, 0, width as i32, height as i32);
        if let Err(err) = buffer.attach_to(surface) {
            eprintln!("# 挂载缓冲失败: {err}");
        }
        layer.commit();
        self.last_draw = Instant::now();
        self.dirty = false;
    }
}

fn spawn_danmaku(room_id: i64, cookies: Cookies, tx: Sender<UiEvent>) {
    std::thread::spawn(move || {
        let Ok(runtime) = danmu_hime::runtime() else {
            let _ = tx.send(UiEvent::Notice("创建 tokio runtime 失败".into()));
            return;
        };
        runtime.block_on(async move {
            let (inner_tx, mut inner_rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(DanmakuClient::new(room_id, cookies).run(inner_tx));
            while let Some(event) = inner_rx.recv().await {
                // 浮层上只显示弹幕；连接状态这些走 stderr，别糊在屏幕上。
                let message = match event {
                    ClientEvent::Connecting { attempt } => {
                        eprintln!(
                            "# 连接中{}…",
                            if attempt > 0 {
                                format!("（第 {attempt} 次重连）")
                            } else {
                                String::new()
                            }
                        );
                        continue;
                    }
                    ClientEvent::Connected { room } => {
                        eprintln!(
                            "# 已连接 房间 {}「{}」 {} 在线 {}",
                            room.room_id,
                            room.title,
                            room.status_text(),
                            room.online
                        );
                        continue;
                    }
                    ClientEvent::Disconnected { reason, retry_in } => {
                        eprintln!(
                            "# 断线：{reason}{}",
                            retry_in
                                .map(|delay| format!("，{} 秒后重连", delay.as_secs()))
                                .unwrap_or_default()
                        );
                        continue;
                    }
                    ClientEvent::Fatal { message } => UiEvent::Notice(format!("连接失败：{message}")),
                    ClientEvent::Danmaku(event) => {
                        // 弹幕自带表情原图的话顺手下一张（同一字符只下一次，之后走缓存）
                        if let DanmakuEvent::Danmaku(danmaku) = &event
                            && let Some(emote) = &danmaku.emote
                        {
                            ensure_emote(&tx, emote_placeholder(&emote.text), &emote.url);
                        }
                        match to_line(event) {
                            Some((prefix, text)) => UiEvent::Danmaku { prefix, text },
                            None => continue,
                        }
                    }
                };
                if tx.send(message).is_err() {
                    break;
                }
            }
        });
    });
}

/// 要不要画粉丝牌子。解析发生在自由函数里，拿不到 self，就用个全局开关。
static SHOW_MEDAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
/// 要不要画礼物（SC 一直画）。
static SHOW_GIFT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// B 站经典小表情（`[dog]` 这种）在 web 客户端里是写死的一张表，没有公开接口
/// （`GetEmoticons` 给的是房间大表情，而且要登录）。这里对着常见的那批做一张
/// 名字 → Unicode 表情的对照表，直接走已经打通的彩色 emoji 渲染路径。
const EMOTE_TABLE: &[(&str, &str)] = &[
    ("dog", "\u{1F436}"),
    ("doge", "\u{1F415}"),
    ("喵", "\u{1F63A}"),
    ("猫", "\u{1F63A}"),
    ("大哭", "\u{1F62D}"),
    ("哭", "\u{1F622}"),
    ("微笑", "\u{1F642}"),
    ("大笑", "\u{1F604}"),
    ("偷笑", "\u{1F92D}"),
    ("呲牙", "\u{1F601}"),
    ("汗", "\u{1F605}"),
    ("抓狂", "\u{1F62B}"),
    ("疑问", "\u{2753}"),
    ("惊讶", "\u{1F632}"),
    ("委屈", "\u{1F97A}"),
    ("白眼", "\u{1F644}"),
    ("打脸", "\u{1F633}"),
    ("鼓掌", "\u{1F44F}"),
    ("抱抱", "\u{1F917}"),
    ("生气", "\u{1F620}"),
    ("怒", "\u{1F621}"),
    ("睡觉", "\u{1F634}"),
    ("饿了", "\u{1F924}"),
    ("好吃", "\u{1F60B}"),
    ("发财", "\u{1F4B0}"),
    ("谢谢", "\u{1F64F}"),
    ("元气", "\u{26A1}"),
    ("星星", "\u{1F31F}"),
    ("爱心", "\u{2764}\u{FE0F}"),
    ("火", "\u{1F525}"),
    ("蛋糕", "\u{1F382}"),
    ("玫瑰", "\u{1F339}"),
    ("色", "\u{1F60D}"),
    ("口罩", "\u{1F637}"),
    ("滑稽", "\u{1F60F}"),
    ("呆", "\u{1F610}"),
    ("衰", "\u{1F61E}"),
    ("囧", "\u{1F626}"),
    ("吃瓜", "\u{1F349}"),
    ("看", "\u{1F440}"),
    ("酸了", "\u{1F34B}"),
    ("惊喜", "\u{1F929}"),
    ("点赞", "\u{1F44D}"),
    ("妙", "\u{1F62E}"),
    ("无语", "\u{1F611}"),
    ("舔屏", "\u{1F924}"),
    ("嫌弃", "\u{1F612}"),
    ("亲亲", "\u{1F618}"),
    ("阴险", "\u{1F642}"),
    ("星星眼", "\u{1F929}"),
    ("大哭大闹", "\u{1F62D}"),
];

/// 官方表情面板（登录后在 web 端能看到的那份）的「名字 → 原图」，构建时嵌进来。
/// 弹幕里没带 url 的情况（本地测试弹幕、某些客户端的弹幕）就靠它。
const EMOTE_SEED_JSON: &str = include_str!("emotes.json");

fn seed_emotes() -> &'static std::collections::HashMap<String, String> {
    static SEED: std::sync::OnceLock<std::collections::HashMap<String, String>> =
        std::sync::OnceLock::new();
    SEED.get_or_init(|| serde_json::from_str(EMOTE_SEED_JSON).unwrap_or_default())
}

/// 这段文字里出现了哪些内置表情（token, url）。用于本地测试弹幕这种没有 url 的场景。
fn seed_emotes_in(text: &str) -> Vec<(String, String)> {
    let seed = seed_emotes();
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        let after = &rest[start + 1..];
        let Some(end) = after.find(']').filter(|end| *end <= 16) else {
            rest = after;
            continue;
        };
        let token = format!("[{}]", &after[..end]);
        if let Some(url) = seed.get(&token)
            && !found.iter().any(|(seen, _): &(String, String)| *seen == token)
        {
            found.push((token, url.clone()));
        }
        rest = &after[end + 1..];
    }
    found
}

/// 表情 token 对应的占位字符。
///
/// - 表里有的（`[dog]` 这种）用表里那个 emoji：字体的 emoji 被 B 站原图顶掉，
///   宽度、基线、缩放全都沿用现成的 emoji 排版，一行里混排也不会错位。
/// - 表里没有的（房间大表情之类）按 token 哈希到私用区字符：主字体没有这个字，
///   走的还是同一条「主字体缺字 → 画位图」的路。
fn emote_placeholder(token: &str) -> char {
    let name = token.trim_start_matches('[').trim_end_matches(']');
    if let Some((_, emoji)) = EMOTE_TABLE.iter().find(|(key, _)| *key == name)
        && let Some(ch) = emoji.chars().next()
    {
        return ch;
    }
    let hash = token
        .bytes()
        .fold(0u32, |acc, byte| acc.wrapping_mul(31).wrapping_add(byte as u32));
    char::from_u32(0xE000 + hash % 0x1900).unwrap_or(char::from_u32(0xE000).unwrap())
}

/// 表情图缓存目录：`$XDG_CACHE_HOME/danmu-hime/emotes`。
fn emote_cache_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".cache"))
        })?;
    Some(base.join("danmu-hime").join("emotes"))
}

/// 拉一张表情图：缓存里有就直接发给主线程，没有就后台下一张、下好再发。
fn ensure_emote(tx: &Sender<UiEvent>, ch: char, url: &str) {
    // 同一个字符只下一次（主线程和弹幕线程共用这一份）
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<char>>> =
        std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
    if !seen.lock().unwrap_or_else(|err| err.into_inner()).insert(ch) {
        return;
    }
    let cached =
        emote_cache_dir().map(|dir| dir.join(format!("{:x}.png", md5::compute(url.as_bytes()))));
    if let Some(path) = &cached
        && let Ok(bytes) = std::fs::read(path)
    {
        let _ = tx.send(UiEvent::Emote {
            ch,
            bytes: std::sync::Arc::new(bytes),
        });
        return;
    }
    // 单独的线程里跑：lib 的 runtime 是 current_thread 的，在别处 spawn 出去
    // 没人驱动它；浮层主线程和弹幕线程又都不适合做阻塞 IO。
    let (tx, url) = (tx.clone(), url.to_string());
    std::thread::spawn(move || {
        let Ok(runtime) = danmu_hime::runtime() else {
            return;
        };
        match runtime.block_on(danmu_hime::api::fetch_image(&url)) {
            Ok(bytes) => {
                if let Some(path) = &cached
                    && let Some(dir) = path.parent()
                {
                    let _ = std::fs::create_dir_all(dir);
                    // 缓存写失败无所谓，内存里这份照样能用
                    let _ = std::fs::write(path, &bytes);
                }
                let _ = tx.send(UiEvent::Emote {
                    ch,
                    bytes: std::sync::Arc::new(bytes),
                });
            }
            Err(error) => eprintln!("# 表情图下载失败：{error}"),
        }
    });
}

/// 把弹幕里的 `[dog]` 之类换成对应的 emoji；表里没有的原样留着。
/// 这条弹幕自带表情原图时（`emote`），表里没有的 token 也换成占位字符——
/// 原图下好之后渲染层会用真图接管这个字符。
fn expand_emotes_with(text: &str, emote: Option<&danmu_hime::protocol::Emote>) -> String {
    // 直播间私有表情发射时正文不带方括号：整条弹幕的正文就是表情名字
    // （例如只有「吃瓜」两个字），图在弹幕自带的 emote 里。
    // 这种情况整条换成占位字符，等原图下好顶上来。
    if let Some(emote) = emote {
        let name = emote.text.trim_start_matches('[').trim_end_matches(']');
        if !name.is_empty() && (text == name || text == format!("[{name}]")) {
            return emote_placeholder(&format!("[{name}]")).to_string();
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find(']').filter(|end| *end <= 16) else {
            out.push('[');
            rest = after;
            continue;
        };
        let name = &after[..end];
        match EMOTE_TABLE.iter().find(|(key, _)| *key == name) {
            Some((_, emoji)) => out.push_str(emoji),
            None => match emote.filter(|emote| {
                // 方括号可有可无，两边都先剥掉再比：公开表情带，直播间私有表情不带
                emote.text.trim_start_matches('[').trim_end_matches(']') == name
            }) {
                // 表里没有，但这条弹幕带了原图：换成占位字符等图
                Some(_) => out.push(emote_placeholder(&format!("[{name}]"))),
                None => {
                    out.push('[');
                    out.push_str(name);
                    out.push(']');
                }
            },
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// 挑我们要画的：弹幕、礼物、醒目留言都成一行；其它事件先不画。
fn to_line(event: DanmakuEvent) -> Option<(String, String)> {
    match &event {
        DanmakuEvent::Danmaku(danmaku) => danmaku_line(danmaku),
        DanmakuEvent::Gift(gift) => {
            if !SHOW_GIFT.load(Ordering::Relaxed) {
                return None;
            }
            Some((
                format!("[礼物] {} ", gift.uname),
                format!("{} ×{}", gift.gift_name, gift.num.max(1)),
            ))
        }
        DanmakuEvent::SuperChat(sc) => Some((
            format!("[SC ¥{}] {}: ", sc.price, sc.uname),
            sc.text.clone(),
        )),
        _ => None,
    }
}

/// 一条弹幕长什么样：`[牌子·等级] (舰长) 昵称: 内容`。
fn danmaku_line(danmaku: &danmu_hime::protocol::Danmaku) -> Option<(String, String)> {
    let mut prefix = String::new();
    if SHOW_MEDAL.load(Ordering::Relaxed)
        && let Some(medal) = &danmaku.medal
    {
        prefix.push_str(&format!("[{}·{}] ", medal.name, medal.level));
    }
    if danmaku.guard > 0 {
        let guard = ["", "总督", "提督", "舰长"][danmaku.guard.min(3) as usize];
        prefix.push_str(&format!("<{guard}> "));
    }
    prefix.push_str(&format!("{}: ", danmaku.uname));
    Some((prefix, expand_emotes_with(&danmaku.text, danmaku.emote.as_ref())))
}

/// 字体：优先 fontconfig（能正确带出 .ttc 的 index），再退回常见路径。
/// 找彩色 emoji 字体。找不到就返回 None（emoji 直接跳过，不影响别的）。
fn find_emoji_font(explicit: Option<&str>) -> Option<(std::path::PathBuf, u32)> {
    if let Some(path) = explicit {
        return Some((std::path::PathBuf::from(path), 0));
    }
    if let Ok(output) = std::process::Command::new("fc-match")
        .args(["-f", "%{file}\t%{index}", "emoji"])
        .output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        if let Some((path, index)) = text.trim().rsplit_once('\t')
            && std::path::Path::new(path).exists()
        {
            return Some((std::path::PathBuf::from(path), index.parse().unwrap_or(0)));
        }
    }
    for candidate in [
        "/usr/share/fonts/noto/NotoColorEmoji.ttf",
        "/usr/share/fonts/noto-emoji/NotoColorEmoji.ttf",
        "/usr/share/fonts/TTF/NotoColorEmoji.ttf",
    ] {
        let path = std::path::PathBuf::from(candidate);
        if path.exists() {
            return Some((path, 0));
        }
    }
    None
}

fn find_font(explicit: Option<&str>) -> Result<(std::path::PathBuf, u32)> {
    if let Some(path) = explicit {
        return Ok((std::path::PathBuf::from(path), 0));
    }
    if let Ok(output) = std::process::Command::new("fc-match")
        .args(["-f", "%{file}\t%{index}", "sans-serif:lang=zh-cn"])
        .output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        if let Some((path, index)) = text.trim().rsplit_once('\t')
            && std::path::Path::new(path).exists()
        {
            return Ok((std::path::PathBuf::from(path), index.parse().unwrap_or(0)));
        }
    }
    for candidate in [
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
    ] {
        let path = std::path::PathBuf::from(candidate);
        if path.exists() {
            return Ok((path, 0));
        }
    }
    bail!("找不到字体，用 --font 指定一个 .ttf/.ttc")
}

impl OutputHandler for Overlay {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl ShmHandler for Overlay {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl CompositorHandler for Overlay {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if new_factor >= 1 && new_factor != self.buffer_scale {
            // 有分数缩放时 wl_surface 的 scale 得保持 1，比例走 viewport。
            if self.viewport.is_none() {
                if std::env::var_os("DANMAKU_DEBUG").is_some() {
                    eprintln!("# 合成器要 buffer_scale={new_factor}");
                }
                self.buffer_scale = new_factor;
                surface.set_buffer_scale(new_factor);
                self.dirty = true;
            }
        }
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        // 没跟合成器要 frame 回调（动画的节拍由定时器管），走不到这儿。
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for Overlay {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if let (Some(width), Some(height)) = (
            std::num::NonZeroU32::new(configure.new_size.0),
            std::num::NonZeroU32::new(configure.new_size.1),
        ) {
            self.width = width.get();
            self.height = height.get();
        }
        self.dirty = true;
        self.draw(qh);
    }
}

delegate_registry!(Overlay);
// SCTK 0.21 起，compositor/shm/layer-shell 这些标准接口的 Dispatch 都由这一个宏搞定
smithay_client_toolkit::delegate_dispatch2!(Overlay);

// 分数缩放/缩放器这两个协议 SCTK 不管，自己接：状态里没有额外数据，用户数据用 ()。
impl Dispatch2<WpFractionalScaleV1, Overlay> for () {
    fn event(
        &self,
        state: &mut Overlay,
        _proxy: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _conn: &Connection,
        _qh: &QueueHandle<Overlay>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && scale != state.fractional120
            && scale > 0
        {
            if std::env::var_os("DANMAKU_DEBUG").is_some() {
                eprintln!("# 合成器要分数缩放 {}/120", scale);
            }
            state.fractional120 = scale;
            state.dirty = true;
        }
    }
}

// 这三个接口没有事件，接上空实现就行（协议要求）。
impl Dispatch2<WpFractionalScaleManagerV1, Overlay> for () {
    fn event(
        &self,
        _state: &mut Overlay,
        _proxy: &WpFractionalScaleManagerV1,
        _event: <WpFractionalScaleManagerV1 as wayland_client::Proxy>::Event,
        _conn: &Connection,
        _qh: &QueueHandle<Overlay>,
    ) {
    }
}

impl Dispatch2<WpViewporter, Overlay> for () {
    fn event(
        &self,
        _state: &mut Overlay,
        _proxy: &WpViewporter,
        _event: <WpViewporter as wayland_client::Proxy>::Event,
        _conn: &Connection,
        _qh: &QueueHandle<Overlay>,
    ) {
    }
}

impl Dispatch2<WpViewport, Overlay> for () {
    fn event(
        &self,
        _state: &mut Overlay,
        _proxy: &WpViewport,
        _event: <WpViewport as wayland_client::Proxy>::Event,
        _conn: &Connection,
        _qh: &QueueHandle<Overlay>,
    ) {
    }
}

impl ProvidesRegistryState for Overlay {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState];
}

/// 从文件里取出新读完的完整行；没写完的那截留着下次再说。
fn take_new_lines(offset: &mut u64, text: &str) -> Vec<String> {
    let start = (*offset as usize).min(text.len());
    let mut consumed = start;
    let mut lines = Vec::new();
    for chunk in text[start..].split_inclusive('\n') {
        let Some(line) = chunk.strip_suffix('\n') else {
            break; // 最后一行还没写完
        };
        consumed += chunk.len();
        let line = line.trim_end_matches('\r');
        if !line.is_empty() {
            lines.push(line.to_string());
        }
    }
    *offset = consumed as u64;
    lines
}

/// 算四个边距：九宫格决定贴哪边，`margin` 是离边距离，`offset` 是再往右/往下推多少。
fn margins_for(anchor: Anchor, margin: i32, offset_x: i32, offset_y: i32) -> (i32, i32, i32, i32) {
    let mut top = margin;
    let mut right = margin;
    let mut bottom = margin;
    let mut left = margin;
    // 正数一律是「离贴着的那条边更远」：贴底就往上挪，贴右就往左挪。
    // （早先按屏幕方向算，贴底的浮层往下推只会推出屏幕，看着像没反应。）
    top += offset_y;
    bottom += offset_y;
    left += offset_x;
    right += offset_x;
    let _ = anchor;
    (top.max(0), right.max(0), bottom.max(0), left.max(0))
}

/// 一条弹幕现在的透明度：`ttl` 之前满不透明，之后在 `fade` 秒里平滑淡到 0。
///
/// 曲线是 1 - smoothstep：刚进淡出期几乎看不出变化，中段明显，尾巴拖得长，
/// 看着像"融化"而不是"到点熄灯"。弹幕姬那摞从上到下的渐变，就是这条曲线
/// 加上比较长的淡出时间（几条之间才拉得开差距）。
fn fade_alpha(age: Duration, ttl: Duration, fade: Duration) -> f32 {
    if age < ttl {
        return 1.0;
    }
    let t = ((age - ttl).as_secs_f32() / fade.as_secs_f32().max(0.001)).clamp(0.0, 1.0);
    (1.0 - (3.0 * t * t - 2.0 * t * t * t)).clamp(0.0, 1.0)
}

/// 这条弹幕是不是已经进了淡出窗口（`ttl` 那一刻开始淡）。
///
/// 这里故意提前一帧算：定时器醒来的时刻是「到点」那一刻，而那一刻
/// `fade_alpha` 还正好等于 1.0（还没开始淡），要是按 alpha 判断就会认为
/// 「没有动画」→ 直接睡到 fade 结束，整段淡出就被跳过去了。
fn fading(age: Duration, ttl: Duration) -> bool {
    // 淡入那 0.25 秒也算"有动画"，否则新弹幕会直接啪一下出现
    age < FADE_IN || age + FRAME_TICK >= ttl
}

/// 新弹幕淡入的时长。
const FADE_IN: Duration = Duration::from_millis(250);
/// 淡完之后收拢这一行占的高度用多久。
const COLLAPSE: Duration = Duration::from_millis(200);

/// 刚出现时的透明度：0.25 秒里从 0 到 1（用 smoothstep，比线性柔）。
fn fade_in_alpha(age: Duration) -> f32 {
    let t = (age.as_secs_f32() / FADE_IN.as_secs_f32()).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 配合 [`fade_alpha`]，下一次需要重绘（或清理）还要等多久。
fn wakeup_for(age: Duration, ttl: Duration, fade: Duration) -> Duration {
    let life = ttl + fade;
    if age < ttl {
        ttl - age
    } else if age < life {
        life - age
    } else {
        Duration::ZERO
    }
}

#[cfg(test)]
mod emote_tests {
    use super::*;

    #[test]
    fn bundled_table_knows_common_emotes() {
        let seed = seed_emotes();
        assert!(seed.len() > 100, "内置表太小了：{}", seed.len());
        for token in ["[doge]", "[大笑]", "[吃瓜]"] {
            let url = seed.get(token).unwrap_or_else(|| panic!("内置表里没有 {token}"));
            assert!(url.ends_with(".png"), "{token} 的 url 不像原图：{url}");
        }
    }

    #[test]
    fn seed_scan_finds_tokens_in_text() {
        let found = seed_emotes_in("好耶[doge]再来一个[大笑]");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, "[doge]");
        assert_eq!(found[1].0, "[大笑]");
        // 没听过的表情、半个方括号都不算
        assert!(seed_emotes_in("看这个[没听过的]和[半个").is_empty());
    }

    #[test]
    fn table_emotes_use_the_emoji_from_the_table() {
        assert_eq!(emote_placeholder("[dog]"), '\u{1F436}');
        assert_eq!(emote_placeholder("[大笑]"), '\u{1F604}');
    }

    #[test]
    fn unknown_emotes_get_a_stable_private_use_char() {
        let ch = emote_placeholder("[tv_doge]");
        assert!((0xE000..=0xF8FF).contains(&(ch as u32)), "应该落在私用区：{ch:?}");
        assert_eq!(ch, emote_placeholder("[tv_doge]"), "同一个 token 每次都该一样");
        assert_ne!(ch, emote_placeholder("[小电视表情]"));
    }

    #[test]
    fn emote_with_image_replaces_unknown_token() {
        let emote = danmu_hime::protocol::Emote {
            text: "[tv_doge]".into(),
            url: "https://i0.hdslb.com/bfs/emote/x.png".into(),
        };
        assert_eq!(
            expand_emotes_with("好耶[tv_doge]！", Some(&emote)),
            format!("好耶{}！", emote_placeholder("[tv_doge]"))
        );
        // 表里有的一律走表，不受这条弹幕的表情影响
        assert_eq!(expand_emotes_with("好耶[dog]", Some(&emote)), "好耶\u{1F436}");
        // 不是这条弹幕带的那个 token 就原样留着
        assert_eq!(expand_emotes_with("好耶[别]表情]", Some(&emote)), "好耶[别]表情]");
    }

    #[test]
    fn emote_cache_dir_sits_under_xdg_cache() {
        let dir = emote_cache_dir().expect("HOME/XDG_CACHE_HOME 总有吧");
        assert!(dir.ends_with("danmu-hime/emotes"), "{dir:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_config_fills_args() {
        let path = std::env::temp_dir().join(format!("danmu-hime-config-{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"{"room":"14709735","font_size":26.5,"line_gap":1.0,"opacity":0.3,
                "anchor":"top-left","layer":"overlay","ttl":5.5,"width":500}"#,
        )
        .expect("写临时配置");
        let config = FileConfig::load(&path).expect("读配置");
        let mut args = Args::default();
        config.apply_to(&mut args).expect("应用配置");
        assert_eq!(args.room, "14709735");
        assert_eq!(args.theme.font_size, 26.5);
        assert_eq!(args.theme.line_gap, 1.0);
        assert_eq!(args.theme.panel_alpha, 0.3);
        assert_eq!(args.anchor, Anchor::TOP | Anchor::LEFT);
        // 层级已经不是配置项了：永远是 overlay 层
        assert_eq!(args.ttl, 5.5);
        assert_eq!(args.width, 500);
        // 没写的项保持默认，不会被抹成 0
        assert_eq!(args.height, 560);
        assert_eq!(args.max_lines, 101);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_danmaku_skips_what_is_already_in_the_file() {
        // 启动时的 offset 就是文件当前长度：旧内容一条都不放
        let mut offset = "旧的\n旧的\n".len() as u64;
        let text = "旧的\n旧的\n新的\n";
        assert_eq!(take_new_lines(&mut offset, text), vec!["新的"]);
    }

    #[test]
    fn test_danmaku_file_reads_new_lines_only() {
        let mut offset = 0;
        let lines = take_new_lines(&mut offset, "第一条\n第二条\n没写完");
        assert_eq!(lines, vec!["第一条", "第二条"]);
        assert_eq!(offset as usize, "第一条\n第二条\n".len());
        // 续上后面那半行
        let text = "第一条\n第二条\n没写完\n第三条\n";
        assert_eq!(take_new_lines(&mut offset, text), vec!["没写完", "第三条"]);
        // 没有新内容就什么都不返回
        assert!(take_new_lines(&mut offset, text).is_empty());
    }

    #[test]
    fn new_lines_fade_in() {
        assert_eq!(fade_in_alpha(Duration::ZERO), 0.0);
        assert!((fade_in_alpha(FADE_IN / 2) - 0.5).abs() < 0.01);
        assert_eq!(fade_in_alpha(FADE_IN), 1.0);
        assert_eq!(fade_in_alpha(Duration::from_secs(5)), 1.0);
        // 刚到手的那一瞬间也要算"有动画"，不然定时器会睡过去
        assert!(fading(Duration::from_millis(10), Duration::from_secs(12)));
    }

    #[test]
    fn offsets_nudge_the_panel() {
        let base = margins_for(Anchor::BOTTOM | Anchor::RIGHT, 16, 0, 0);
        assert_eq!(base, (16, 16, 16, 16));
        // 正数 = 往右/往下：右锚点就减小右边距，下锚点减小下边距
        // 正数 = 离贴的边更远：贴右下就是往上、往左各挪一点
        let pushed = margins_for(Anchor::BOTTOM | Anchor::RIGHT, 16, 6, 10);
        assert_eq!(pushed, (26, 22, 26, 22));
        // 负过头就夹在 0（正好贴边）
        assert_eq!(margins_for(Anchor::BOTTOM | Anchor::RIGHT, 16, -999, -999), (0, 0, 0, 0));
        // 贴左上同样：正数 = 往右下挪
        let other = margins_for(Anchor::TOP | Anchor::LEFT, 16, 30, 20);
        assert_eq!(other, (36, 46, 36, 46));
        // 正数往内挪不会越界；负过头才夹回 0（正好贴边）
        assert_eq!(margins_for(Anchor::BOTTOM | Anchor::RIGHT, 16, 999, 999), (1015, 1015, 1015, 1015));
        assert_eq!(margins_for(Anchor::BOTTOM | Anchor::RIGHT, 16, -4, -6), (10, 12, 10, 12));
    }

    #[test]
    fn emote_tokens_become_emoji() {
        assert_eq!(expand_emotes_with("好耶[dog]", None), "好耶\u{1F436}");
        assert_eq!(expand_emotes_with("[大笑][大笑]", None), "\u{1F604}\u{1F604}");
        // 表里没有的、以及没闭合的方括号，原样留着
        assert_eq!(expand_emotes_with("看这个[没听过的表情]", None), "看这个[没听过的表情]");
        assert_eq!(expand_emotes_with("半个[方括号", None), "半个[方括号");
        assert_eq!(expand_emotes_with("没有表情", None), "没有表情");
    }

    #[test]
    fn integer_fields_take_ints_floats_and_strings() {
        let dir = std::env::temp_dir().join("danmu-hime-i32");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ok.json");
        std::fs::write(
            &path,
            r#"{"room":"1","width":420.0,"height":"560","margin":16.4,"max_lines":42.6}"#,
        )
        .unwrap();
        let config = FileConfig::load_if_exists(&path).unwrap();
        assert_eq!(config.width, Some(420));
        assert_eq!(config.height, Some(560));
        assert_eq!(config.margin, Some(16));
        assert_eq!(config.max_lines, Some(43));
    }

    #[test]
    fn broken_config_reports_an_error_instead_of_panicking() {
        let dir = std::env::temp_dir().join("danmu-hime-broken");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.json");
        std::fs::write(&path, r#"{"room": "14709735", "width": }"#).unwrap();
        assert!(FileConfig::load_if_exists(&path).is_err());
    }

    #[test]
    fn missing_config_is_not_an_error() {
        let config = FileConfig::load_if_exists(Path::new("/nonexistent/danmu-hime-config.json"))
            .expect("文件不存在不算错");
        assert!(config.room.is_none() && config.font_size.is_none());
    }

    #[test]
    fn alpha_holds_then_fades() {
        let (ttl, fade) = (Duration::from_secs(12), Duration::from_secs(1));
        assert_eq!(fade_alpha(Duration::ZERO, ttl, fade), 1.0);
        assert_eq!(fade_alpha(Duration::from_secs(11), ttl, fade), 1.0);
        assert_eq!(fade_alpha(Duration::from_secs(12), ttl, fade), 1.0);
        let half = fade_alpha(Duration::from_millis(12_500), ttl, fade);
        assert!((half - 0.5).abs() < 0.01, "淡到一半时应该是 0.5，实际 {half}");
        // 平滑曲线：前四分之一还几乎满亮，后四分之一已经快没了
        assert!(fade_alpha(Duration::from_millis(12_250), ttl, fade) > 0.8);
        assert!(fade_alpha(Duration::from_millis(12_750), ttl, fade) < 0.2);
        assert_eq!(fade_alpha(Duration::from_secs(13), ttl, fade), 0.0);
        assert_eq!(fade_alpha(Duration::from_secs(99), ttl, fade), 0.0);
    }

    #[test]
    fn fade_window_includes_the_boundary() {
        let (ttl, fade) = (Duration::from_secs(12), Duration::from_secs(1));
        // 还没到 ttl：不该按帧刷
        assert!(!fading(ttl - Duration::from_millis(500), ttl));
        // 正好到点（alpha 还是 1.0）和淡出中：都得按帧刷，否则整段淡出被跳过
        assert!(fading(ttl, ttl));
        assert!(fading(ttl + fade / 2, ttl));
        assert!(fading(ttl + fade, ttl));
    }

    #[test]
    fn wakeup_follows_ttl_then_fade_end() {
        let (ttl, fade) = (Duration::from_secs(12), Duration::from_secs(1));
        assert_eq!(wakeup_for(Duration::ZERO, ttl, fade), ttl);
        assert_eq!(
            wakeup_for(Duration::from_millis(11_500), ttl, fade),
            Duration::from_millis(500)
        );
        // 淡出中：下次醒是它该被清掉的时候（中间那些帧由 frame 回调负责）
        assert_eq!(
            wakeup_for(Duration::from_millis(12_400), ttl, fade),
            Duration::from_millis(600)
        );
        assert_eq!(wakeup_for(Duration::from_secs(30), ttl, fade), Duration::ZERO);
    }
}
