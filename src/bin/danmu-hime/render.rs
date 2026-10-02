//! 软件渲染：一块通铺的暗色底板 + 上面的弹幕文字，ab_glyph 把字刷进像素缓冲。
//!
//! - 画布默认就是全透明的：没有弹幕时这一层「什么都没有」。
//! - 底板是**一整块固定宽度的方形半透明矩形**（不是每条弹幕一个圆角气泡），
//!   高度刚好裹住当前可见的那几行，随内容一起出现、一起淡出。
//! - 新的弹幕贴底，旧的往上推，超出高度上限的（更老的）不画；
//!   推的过程由调用方给一个逐帧衰减的 `scroll` 位移，看起来是滑上去而不是瞬移。
//! - 一条弹幕太长就在底板宽度内**软换行**（汉字任意断、拉丁词整词挪走），
//!   续行缩进一格，底板高度按视觉行数算。

use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use tiny_skia::{Color, Paint, Pixmap, Rect, Shader, Transform};

use crate::png;

/// 行类型：现在只有弹幕和出错提示（礼物、醒目留言以后再加）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Danmaku,
    System,
}

/// 一帧里要画的一行；存活时长/淡出透明度由调用方算好。
#[derive(Debug, Clone)]
pub struct DrawLine {
    pub kind: Kind,
    pub prefix: String,
    pub text: String,
    /// 0..1，淡出时连底板上这一行一起变淡。
    pub alpha: f32,
    /// 0..1 入场进度：1 = 已经在位置上，小于 1 的话这一条从下面滑上来。
    pub enter: f32,
    /// 0..1 收拢进度：淡完之后占的高度从一行收到 0，让上面的行滑下去而不是跳。
    pub collapse: f32,
}

pub struct Theme {
    pub font_size: f32,
    /// 底板的不透明度；0 就是只有字。
    pub panel_alpha: f32,
    /// 两行之间的空隙（底板上仍然是连着的）。
    pub line_gap: f32,
    /// 昵称（和牌子）的颜色。
    pub name_color: (u8, u8, u8),
    /// 弹幕内容的颜色。
    pub text_color: (u8, u8, u8),
    /// 底板底色。
    pub panel_rgb: (u8, u8, u8),

}

impl Default for Theme {
    fn default() -> Self {
        Self {
            font_size: 30.0,
            panel_alpha: 0.6,
            name_color: (255, 229, 138),
            text_color: (255, 255, 255),
            panel_rgb: (0, 0, 0),
            line_gap: 4.0,
        }
    }
}

pub struct Renderer {
    /// 字体用 mmap 而不是整个读进来：NotoSansCJK 有 19MB，读进来常驻内存就炸了。
    font: memmap2::Mmap,
    font_index: u32,
    /// 设备像素比：合成器给的分数缩放，按它放大渲染才不糊。
    pub scale: f32,
    pub theme: Theme,
    /// 字号/间距的基准值，`scale` 变了从这里重算。
    base_font_size: f32,
    base_line_gap: f32,
    /// 彩色 emoji 字体（Noto Color Emoji 这类 CBDT 位图字体，可选）。
    emoji_font: Option<(memmap2::Mmap, u32)>,
    /// B 站弹幕表情（`[dog]` 这种）的原图，挂在占位字符上。
    /// 有图之后这个字符就不再用字体里的 emoji，直接贴原图。
    emote_sources: RefCell<HashMap<char, Rc<png::Image>>>,
    /// 解好、缩好的 emoji 位图。按「字符 + 大小」缓存：每帧现解 PNG 太慢。
    emoji_cache: EmojiCache,
    /// 用户要的整体放大倍数（GUI 的「缩放」滑块）。
    zoom: f32,
}

/// 缓存键：字符 + 目标宽度（按 1/4 像素取整，够用了）。
type EmojiKey = (char, u16);
type EmojiCache = RefCell<HashMap<EmojiKey, Option<Rc<Emoji>>>>;

/// 缩到目标尺寸的 emoji 位图（RGBA8，非预乘）。
struct Emoji {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Renderer {
    pub fn new(font: memmap2::Mmap, font_index: u32, theme: Theme) -> Self {
        let base_font_size = theme.font_size;
        let base_line_gap = theme.line_gap;
        Self {
            font,
            font_index,
            scale: 1.0,
            theme,
            base_font_size,
            base_line_gap,
            zoom: 1.0,
            emoji_font: None,
            emoji_cache: RefCell::new(HashMap::new()),
            emote_sources: RefCell::new(HashMap::new()),
        }
    }

    /// 挂上彩色 emoji 字体（可选）。没挂的话画不出来的字符直接跳过，不留豆腐块。
    /// 整体放大倍数（1.0 = 原样）。跟设备像素比是两回事：这个只放大字和行距。
    pub fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom.clamp(0.2, 5.0);
        self.theme.font_size = self.base_font_size * self.scale * self.zoom;
        self.theme.line_gap = self.base_line_gap * self.scale * self.zoom;
    }

    /// 收下一张 B 站表情原图，挂到占位字符上（`[dog]` → 字体里那个 🐶）。
    /// 返回 false 说明这张图解不开（不是 png 之类），跳过就行。
    pub fn load_emote(&self, ch: char, bytes: &[u8]) -> bool {
        let Ok(image) = png::decode(bytes) else {
            return false;
        };
        if image.width == 0 || image.height == 0 {
            return false;
        }
        self.emote_sources.borrow_mut().insert(ch, Rc::new(image));
        // 同一个字符之前可能按「字体 emoji」缓存过，尺寸也不一样，全丢掉
        self.emoji_cache
            .borrow_mut()
            .retain(|(cached, _), _| *cached != ch);
        true
    }

    pub fn set_emoji_font(&mut self, font: memmap2::Mmap, index: u32) {
        self.emoji_font = Some((font, index));
        self.emoji_cache.borrow_mut().clear();
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale.max(1.0);
        self.theme.font_size = self.base_font_size * self.scale * self.zoom;
        self.theme.line_gap = self.base_line_gap * self.scale * self.zoom;
    }

    /// 改字号基准值（热重载用）：当前缩放倍数不变，立刻按新字号重排。
    pub fn set_base_font_size(&mut self, size: f32) {
        if size > 0.0 {
            self.base_font_size = size;
            self.theme.font_size = self.base_font_size * self.scale * self.zoom;
        }
    }

    /// 改行距基准值（热重载用）。
    pub fn set_base_line_gap(&mut self, gap: f32) {
        if gap >= 0.0 {
            self.base_line_gap = gap;
            self.theme.line_gap = self.base_line_gap * self.scale * self.zoom;
        }
    }

    /// 一行占的高度（文字行高 + 行距）。
    pub fn step(&self) -> f32 {
        self.line_height() + self.theme.line_gap
    }

    fn line_height(&self) -> f32 {
        (self.theme.font_size * 1.15).round()
    }

    /// 这么高的浮层最多站得下几条。
    pub fn capacity(&self, height: u32) -> usize {
        (height as f32 / self.step()).floor().max(1.0) as usize
    }

    /// 画一帧。`lines` 按时间顺序（老的在前），新的画在最下面。
    ///
    /// `scroll` 是整体往下的位移（设备像素）：新弹幕进来时整摞先被压下去一点，
    /// 再随时间回到 0，看起来就是「被顶上去」的过渡，而不是瞬移。
    pub fn render(&self, width: u32, height: u32, lines: &[DrawLine], scroll: f32) -> Vec<u8> {
        let mut pixmap = match Pixmap::new(width, height) {
            Some(pixmap) => pixmap,
            None => return vec![0; (width * height * 4) as usize],
        };
        let visible: Vec<&DrawLine> = lines.iter().filter(|line| line.alpha > 0.01).collect();
        if !visible.is_empty()
            && let Ok(font) = FontRef::try_from_slice_and_index(&self.font, self.font_index)
        {
            self.draw_content(&mut pixmap, &font, &visible, scroll.max(0.0));
        }
        let mut pixels = pixmap.take();
        // tiny-skia 的 RGBA8888 在内存里是 [R,G,B,A]，wl_shm 的 Argb8888 是
        // [B,G,R,A]，字节序反着；不换的话蓝色会画成橙色。
        for px in pixels.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        pixels
    }

    /// 把要画的弹幕排成视觉行：一条弹幕太长就在底板宽度里软换行，
    /// 续行缩进一格（不缩进的话，两条挨着就分不清是不是同一个人的了）。
    fn layout_rows(&self, font: &FontRef<'_>, width: f32, lines: &[&DrawLine]) -> Vec<Row> {
        let size = self.theme.font_size;
        let pad_x = (size * 0.7).round();
        // 续行不再缩进：跟第一条左对齐，长弹幕看起来更像一整块
        let indent = 0.0f32;
        let inner = (width - pad_x * 2.0).max(size);
        let mut rows = Vec::new();
        for line in lines {
            let prefix_width = measure_text(font, size, &line.prefix);
            let chunks = wrap_rows(
                font,
                size,
                &line.text,
                (inner - prefix_width).max(size),
                (inner - indent).max(size),
            );
            for (index, chunk) in chunks.into_iter().enumerate() {
                rows.push(Row {
                    kind: line.kind,
                    alpha: line.alpha,
                    prefix: if index == 0 {
                        line.prefix.clone()
                    } else {
                        String::new()
                    },
                    indent: if index == 0 { 0.0 } else { indent },
                    text: chunk,
                    enter: line.enter,
                    collapse: line.collapse,
                });
            }
        }
        rows
    }

    /// 一条弹幕（含前缀）在这块画布上要占几个视觉行（长弹幕换行后会 >1）。
    /// 调用方按它来凑高度上限，而不是按条数。
    pub fn rows_of(&self, width: u32, prefix: &str, text: &str) -> usize {
        let Ok(font) = FontRef::try_from_slice_and_index(&self.font, self.font_index) else {
            return 1;
        };
        let line = DrawLine {
            kind: Kind::Danmaku,
            prefix: prefix.to_string(),
            text: text.to_string(),
            alpha: 1.0,
            enter: 1.0,
                collapse: 1.0,
            };
        self.layout_rows(&font, width as f32, &[&line]).len().max(1)
    }

    fn draw_content(
        &self,
        pixmap: &mut Pixmap,
        font: &FontRef<'_>,
        lines: &[&DrawLine],
        scroll: f32,
    ) {
        let size = self.theme.font_size;
        let line_height = self.line_height();
        let gap = self.theme.line_gap;
        let pad_x = (size * 0.7).round();
        let pad_y = (size * 0.3).round();
        let width = pixmap.width() as f32;
        let height = pixmap.height() as f32;

        let rows = self.layout_rows(font, width, lines);

        // 底板：整宽、方角、暗色半透明，高度刚好裹住这几行
        // （最上面一行淡出时整块跟着变淡，全没了就什么都不剩）。
        // 整块随 scroll 往下偏，超出画布下沿的部分自然被裁掉——新弹幕就是
        // 这么从屏幕下沿挤进来的。
        let count = rows.len() as f32;
        let heights: f32 = rows
            .iter()
            .map(|row| line_height * row.collapse.clamp(0.0, 1.0))
            .sum();
        let panel_height = heights + (count - 1.0) * gap + pad_y * 2.0;
        let panel_top = height + scroll - panel_height;
        let panel_alpha = lines
            .iter()
            .map(|line| line.alpha.clamp(0.0, 1.0))
            .fold(0.0, f32::max);
        if panel_alpha > 0.01
            && let Some(rect) = Rect::from_xywh(0.0, panel_top.max(0.0), width, panel_height)
        {
            let paint = Paint {
                anti_alias: true,
                shader: Shader::SolidColor(Color::from_rgba8(
                    self.theme.panel_rgb.0,
                    self.theme.panel_rgb.1,
                    self.theme.panel_rgb.2,
                    (self.theme.panel_alpha.clamp(0.0, 1.0) * panel_alpha * 255.0).round() as u8,
                )),
                ..Default::default()
            };
            pixmap.fill_rect(rect, &paint, Transform::identity(), None);
        }

        // 文字：最老的排最上面，新的贴底（整体左对齐，真装不下就裁掉）
        let mut bottom = height + scroll - pad_y;
        let enter_shift = self.step();
        for row in rows.iter().rev() {
            let row_h = line_height * row.collapse.clamp(0.0, 1.0);
            if row_h <= 0.5 {
                // 已经收完了：留着占位没意义，往下继续排
                bottom -= gap;
                continue;
            }
            let top = bottom - row_h;
            if top < 0.0 {
                break;
            }
            // 新来的那条：还差多少入场，就往下偏多少（画布下沿自然裁掉）
            let slide = (1.0 - row.enter.clamp(0.0, 1.0)) * enter_shift;
            let alpha = row.alpha.clamp(0.0, 1.0);
            let baseline = top + slide + row_h * 0.5 + size * 0.35;
            let mut cursor = pad_x + row.indent;
            let max_x = width - pad_x;
            if !row.prefix.is_empty() {
                self.draw_text(
                    pixmap,
                    font,
                    size,
                    &mut cursor,
                    baseline,
                    self.theme.prefix_color(row.kind),
                    alpha,
                    &row.prefix,
                    max_x,
                );
            }
            self.draw_text(
                pixmap,
                font,
                size,
                &mut cursor,
                baseline,
                self.theme.text_color(row.kind),
                alpha,
                &row.text,
                max_x,
            );
            bottom = top - gap;
        }
    }
}

/// 切开后的一个视觉行（一条弹幕可能占好几行）。
struct Row {
    kind: Kind,
    alpha: f32,
    collapse: f32,
    /// 0..1 入场进度（新弹幕从下面滑上来）
    enter: f32,
    /// 只有第一行带前缀，续行是空的（并且缩进）。
    prefix: String,
    indent: f32,
    text: String,
}

/// 量一段文字画出来有多宽（推进方式和 [`draw_text`] 保持一致）。
fn measure_text(font: &FontRef<'_>, size: f32, text: &str) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut width = 0.0;
    let mut prev: Option<GlyphId> = None;
    for ch in text.chars() {
        let id = scaled.glyph_id(ch);
        width += scaled.h_advance(id) + prev.map_or(0.0, |last| scaled.kern(last, id));
        prev = Some(id);
    }
    width
}

/// 按宽度把文字切成若干行。汉字可以任意断，拉丁词尽量整词挪到下一行；
/// 一个词本身就超宽的话只能硬断。
fn wrap_rows(
    font: &FontRef<'_>,
    size: f32,
    text: &str,
    first_width: f32,
    rest_width: f32,
) -> Vec<String> {
    let scaled = font.as_scaled(PxScale::from(size));
    let chars: Vec<char> = text.chars().collect();
    // 每个字的步进（含和上一个字的 kern）
    let mut steps = Vec::with_capacity(chars.len());
    let mut prev: Option<GlyphId> = None;
    for ch in &chars {
        let id = scaled.glyph_id(*ch);
        steps.push(scaled.h_advance(id) + prev.map_or(0.0, |last| scaled.kern(last, id)));
        prev = Some(id);
    }

    let mut rows = Vec::new();
    let mut start = 0usize;
    let mut limit = first_width.max(1.0);
    let mut last_break = 0usize;
    let mut width = 0.0f32;
    let mut index = 0usize;
    while index < chars.len() {
        if width + steps[index] > limit && index > start {
            // 优先在上一个断点断开，这行里没断点就硬断
            let mut cut = if last_break > start { last_break } else { index };
            // 禁则：别让标点落到下一行的行首（「、」「。」这些）
            while cut > start + 1 && chars.get(cut).is_some_and(|ch| cannot_start_line(*ch)) {
                cut -= 1;
            }
            rows.push(chars[start..cut].iter().collect::<String>().trim_end().to_string());
            start = cut;
            while start < index && chars[start] == ' ' {
                start += 1;
            }
            limit = rest_width.max(1.0);
            last_break = start;
            width = steps[start..index].iter().sum();
            continue;
        }
        width += steps[index];
        if chars[index] == ' '
            || chars[index].is_ascii_punctuation()
            || chars
                .get(index + 1)
                .is_some_and(|next| is_cjk(*next) && !cannot_start_line(*next))
        {
            last_break = index + 1;
        }
        index += 1;
    }
    rows.push(chars[start..].iter().collect());
    rows
}

/// 这些标点不能出现在行首（不然读起来像断错了）。
fn cannot_start_line(ch: char) -> bool {
    matches!(
        ch,
        '、' | '。' | '，' | '．' | '！' | '？' | '：' | '；' | '）' | '」' | '』' | '】' | '》'
            | '〉' | '·' | '～' | '…' | '—' | '％' | '‰' | '℃'
    )
}

/// 能不能在它前面断开（汉字、假名、谚文、全角标点都不怕拆）。
fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60
    )
}

impl Theme {
    /// 昵称/牌子那一截的颜色。改主题就是改这里。
    fn prefix_color(&self, kind: Kind) -> (u8, u8, u8) {
        match kind {
            Kind::Danmaku => self.name_color,
            Kind::System => (120, 220, 180),
        }
    }

    /// 正文颜色。
    fn text_color(&self, kind: Kind) -> (u8, u8, u8) {
        match kind {
            Kind::Danmaku => self.text_color,
            Kind::System => (190, 245, 215),
        }
    }
}

impl Renderer {
/// 把一段文字刷到 `*x` 处（基线 baseline），到 `max_x` 就截住。
/// 参数就是渲染需要的那几样，串成结构体反而更绕，这里放开 lint。
#[allow(clippy::too_many_arguments)]
fn draw_text(
    &self,
    pixmap: &mut Pixmap,
    font: &FontRef<'_>,
    size: f32,
    x: &mut f32,
    baseline: f32,
    rgb: (u8, u8, u8),
    alpha: f32,
    text: &str,
    max_x: f32,
) -> bool {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut prev = None;
    let mut truncated = false;
    for ch in text.chars() {
        if *x > max_x {
            truncated = true;
            break;
        }
        let id = scaled.glyph_id(ch);
        if let Some(prev) = prev {
            *x += scaled.kern(prev, id);
        }
        let mut advance = scaled.h_advance(id);
        // 挂了 B 站原图的字符（含哈希出来的私用区占位符）一律先画原图：
        // 主字体里有没有这个字都无所谓，宽度统一按一个 em 算，免得字体给 0 宽度。
        if self.emote_sources.borrow().contains_key(&ch) {
            // 这个字符画的是 B 站原图，宽度就按一个汉字（1em）算：
            // 私用区字符的 .notdef 宽度由字体决定（实测能到 2.3em），
            // 拿它当宽度会把表情拉得又宽又扁。
            advance = size;
            self.draw_emoji(pixmap, ch, *x, baseline, advance, alpha);
            *x += advance;
            prev = Some(id);
            continue;
        }
        if id.0 == 0 {
            // 主字体没这个字：大概率是 emoji，去彩色字体里找
            self.draw_emoji(pixmap, ch, *x, baseline, advance, alpha);
            *x += advance;
            prev = Some(id);
            continue;
        }
        if let Some(outline) = scaled.outline_glyph(scaled.scaled_glyph(ch)) {
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, coverage| {
                let px = *x + bounds.min.x + gx as f32;
                let py = baseline + bounds.min.y + gy as f32;
                blend(
                    pixmap,
                    px.round() as i32,
                    py.round() as i32,
                    rgb,
                    coverage * alpha,
                );
            });
        }
        *x += advance;
        prev = Some(id);
    }
    truncated
}
}

impl Renderer {
    /// 画一个彩色 emoji：横向居中在这个字的宽度里，纵向跟汉字一样坐在基线上。
    fn draw_emoji(
        &self,
        pixmap: &mut Pixmap,
        ch: char,
        x: f32,
        baseline: f32,
        advance: f32,
        alpha: f32,
    ) {
        let max_width = advance.max(4.0);
        // 有 B 站原图就用原图，没有才去字体里找彩色 emoji
        let image = self.emote_image(ch, max_width).or_else(|| {
            let (font, index) = self.emoji_font.as_ref()?;
            self.emoji_image(font, *index, ch, max_width)
        });
        let Some(image) = image else {
            return;
        };
        let left = x + (advance - image.width as f32) * 0.5;
        // 汉字大致从基线往上 0.88em，emoji 就对齐这个底线
        let top = baseline + advance * 0.12 - image.height as f32;
        blit(pixmap, left, top, &image, alpha);
    }

    /// 取（并缓存）一张缩好的 emoji 位图。
    fn emoji_image(
        &self,
        font: &memmap2::Mmap,
        index: u32,
        ch: char,
        max_width: f32,
    ) -> Option<Rc<Emoji>> {
        let key = (ch, (max_width * 4.0).round().clamp(1.0, 4096.0) as u16);
        if let Some(hit) = self.emoji_cache.borrow().get(&key) {
            return hit.clone();
        }
        let built = self.build_emoji(font, index, ch, max_width).map(Rc::new);
        let mut cache = self.emoji_cache.borrow_mut();
        if cache.len() >= 512 {
            cache.clear();
        }
        cache.insert(key, built.clone());
        built
    }

    /// 取（并缓存）一张缩好的 B 站表情位图；这个字符没挂表情就返回 None。
    /// 缩放口径跟 [`Renderer::build_emoji`] 一致：按「别超过这个字的宽度」来。
    fn emote_image(&self, ch: char, max_width: f32) -> Option<Rc<Emoji>> {
        let key = (ch, (max_width * 4.0).round().clamp(1.0, 4096.0) as u16);
        if let Some(hit) = self.emoji_cache.borrow().get(&key) {
            return hit.clone();
        }
        let source = self.emote_sources.borrow().get(&ch).cloned()?;
        // 等比缩放：按长边缩，长条形的大表情也不会被拉变形
        let factor = max_width / source.width.max(source.height) as f32;
        let width = ((source.width as f32 * factor).round() as u32).max(1);
        let height = ((source.height as f32 * factor).round() as u32).max(1);
        let built = Some(Rc::new(Emoji {
            width,
            height,
            rgba: resize(&source, width, height),
        }));
        let mut cache = self.emoji_cache.borrow_mut();
        if cache.len() >= 512 {
            cache.clear();
        }
        cache.insert(key, built.clone());
        built
    }

    fn build_emoji(
        &self,
        font: &memmap2::Mmap,
        index: u32,
        ch: char,
        max_width: f32,
    ) -> Option<Emoji> {
        let font = FontRef::try_from_slice_and_index(font, index).ok()?;
        let id = font.glyph_id(ch);
        if id.0 == 0 {
            return None;
        }
        let image = font.glyph_raster_image2(id, max_width.round() as u16)?;
        let png = png::decode(image.data).ok()?;
        // 按「别超过这个字的宽度」缩放（emoji 位图一般比一个 em 宽一点）
        let factor = max_width / image.width as f32;
        let width = ((image.width as f32 * factor).round() as u32).max(1);
        let height = ((image.height as f32 * factor).round() as u32).max(1);
        Some(Emoji {
            width,
            height,
            rgba: resize(&png, width, height),
        })
    }
}

/// 缩放：按面积平均（缩小时够用），在预乘空间里平均，免得边缘发黑。
fn resize(src: &png::Image, width: u32, height: u32) -> Vec<u8> {
    let mut out = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        let y0 = y as u64 * src.height as u64 / height as u64;
        let y1 = (((y + 1) as u64 * src.height as u64).div_ceil(height as u64)).max(y0 + 1);
        for x in 0..width {
            let x0 = x as u64 * src.width as u64 / width as u64;
            let x1 = (((x + 1) as u64 * src.width as u64).div_ceil(width as u64)).max(x0 + 1);
            let mut sums = [0u32; 4];
            let mut count = 0u32;
            for sy in y0..y1.min(src.height as u64) {
                for sx in x0..x1.min(src.width as u64) {
                    let index = ((sy as u32 * src.width + sx as u32) * 4) as usize;
                    let alpha = src.rgba[index + 3] as u32;
                    sums[0] += src.rgba[index] as u32 * alpha / 255;
                    sums[1] += src.rgba[index + 1] as u32 * alpha / 255;
                    sums[2] += src.rgba[index + 2] as u32 * alpha / 255;
                    sums[3] += alpha;
                    count += 1;
                }
            }
            let count = count.max(1);
            let alpha = sums[3] / count;
            let index = ((y * width + x) * 4) as usize;
            if alpha == 0 {
                continue;
            }
            for channel in 0..3 {
                let premultiplied = sums[channel] / count;
                out[index + channel] = (premultiplied * 255 / alpha).min(255) as u8;
            }
            out[index + 3] = alpha as u8;
        }
    }
    out
}

/// 把 emoji 贴上去（source-over，跟着淡出一起变淡）。
fn blit(pixmap: &mut Pixmap, left: f32, top: f32, image: &Emoji, alpha: f32) {
    let (ox, oy) = (left.round() as i32, top.round() as i32);
    for y in 0..image.height as i32 {
        for x in 0..image.width as i32 {
            let index = ((y as u32 * image.width + x as u32) * 4) as usize;
            let coverage = image.rgba[index + 3] as f32 / 255.0 * alpha;
            if coverage <= 0.0 {
                continue;
            }
            blend(
                pixmap,
                ox + x,
                oy + y,
                (image.rgba[index], image.rgba[index + 1], image.rgba[index + 2]),
                coverage,
            );
        }
    }
}

/// 预乘 alpha 的 source-over 混合。
fn blend(pixmap: &mut Pixmap, x: i32, y: i32, rgb: (u8, u8, u8), alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let (width, height) = (pixmap.width() as i32, pixmap.height() as i32);
    if x < 0 || y < 0 || x >= width || y >= height {
        return;
    }
    let alpha = alpha.min(1.0);
    let inv = 1.0 - alpha;
    let index = ((y as u32 * width as u32 + x as u32) * 4) as usize;
    let data = pixmap.data_mut();
    for (offset, channel) in [rgb.0, rgb.1, rgb.2].into_iter().enumerate() {
        data[index + offset] =
            (channel as f32 * alpha + data[index + offset] as f32 * inv).round() as u8;
    }
    data[index + 3] = (255.0 * alpha + data[index + 3] as f32 * inv)
        .round()
        .min(255.0) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> (memmap2::Mmap, u32) {
        let path = std::path::Path::new("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc");
        let file = std::fs::File::open(path).expect("测试要装 noto-cjk 字体");
        // SAFETY: 测试里字体文件不会中途被改。
        let map = unsafe { memmap2::Mmap::map(&file) }.expect("mmap 字体");
        (map, 0)
    }

    /// 8×8 纯红 PNG：给表情渲染测试用，免得依赖磁盘上的图片。
    const RED_PNG: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x08, 0x08, 0x06, 0x00, 0x00, 0x00, 0xc4, 0x0f, 0xbe, 0x8b, 0x00, 0x00, 0x00, 0x12, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x1f, 0x66, 0x18, 0x19, 0x0a, 0x00, 0xc2, 0xd7, 0x7f, 0x81, 0xd5, 0x03, 0x32, 0xfd, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82];

    fn renderer() -> Renderer {
        let (data, index) = font();
        Renderer::new(data, index, Theme::default())
    }

    #[test]
    fn loaded_emote_replaces_the_placeholder_glyph() {
        let renderer = renderer();
        let lines = [line("前\u{E0F1}后")];
        let before = renderer.render(240, 60, &lines, 0.0);
        assert!(renderer.load_emote('\u{E0F1}', RED_PNG), "这张 PNG 应该能解开");
        let after = renderer.render(240, 60, &lines, 0.0);
        assert_ne!(before, after, "挂上原图之后画面应该变了");
        // 渲染输出换过 R/B（wl_shm 是 BGRA），所以红色看第 2 个字节
        let red = after
            .chunks_exact(4)
            .filter(|px| px[2] > 200 && px[0] < 80 && px[3] > 200)
            .count();
        assert!(red > 20, "红色像素太少（{red}），原图没画上去");
    }

    /// 量一下真表情画出来的墨迹范围：本地缓存里有就用真的，没有就跳过。
    #[test]
    fn emote_ink_stays_within_its_box() {
        let home = std::env::var("HOME").unwrap_or_default();
        let mut file = None;
        if let Ok(entries) = std::fs::read_dir(format!("{home}/.cache/danmu-hime/emotes")) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|ext| ext == "png") {
                    file = std::fs::read(entry.path()).ok();
                    if file.is_some() {
                        break;
                    }
                }
            }
        }
        let Some(bytes) = file else {
            return; // 没缓存就算了（别人的机器上不会有）
        };
        let renderer = renderer();
        let ch = '\u{E0F1}';
        assert!(renderer.load_emote(ch, &bytes), "缓存里的表情应该能解开");
        let (width, height) = (400usize, 60u32);
        let pixels = renderer.render(width as u32, height, &[line(&ch.to_string())], 0.0);
        let (mut min_x, mut max_x, mut min_y, mut max_y) = (usize::MAX, 0usize, usize::MAX, 0usize);
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            // 只看亮的像素：底板是黑色半透明，会被这一条排除掉
            if px[3] > 200 && (px[0] as u32 + px[1] as u32 + px[2] as u32) > 240 {
                let (x, y) = (i % width, i / width);
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
        println!(
            "表情墨迹：x {min_x}..={max_x}（宽 {}） y {min_y}..={max_y}（高 {}）",
            max_x - min_x + 1,
            max_y - min_y + 1
        );
        assert!(min_x >= 18, "表情左边跑出内边距了：{min_x}（内边距 21px）");
    }

    #[test]
    fn bad_emote_bytes_are_ignored() {
        let renderer = renderer();
        assert!(!renderer.load_emote('\u{E0F2}', b"not a png"));
        let lines = [line("\u{E0F2}")];
        let with_bad = renderer.render(120, 40, &lines, 0.0);
        let empty = renderer.render(120, 40, &[line("")], 0.0);
        assert_eq!(with_bad, empty, "解不开的图应该什么都不画");
    }

    #[test]
    fn wrap_keeps_every_char_and_respects_width() {
        let (data, index) = font();
        let font = FontRef::try_from_slice_and_index(&data, index).unwrap();
        let text = "这是一条特别长的弹幕，试试软换行能不能把字都塞进底板的宽度里面，                    而且不要把那个拉丁词 wrap 从中间劈开，一个词得整个挪到下一行去";
        let (first, rest) = (300.0, 260.0);
        let rows = wrap_rows(&font, 20.0, text, first, rest);
        assert!(rows.len() >= 3, "长句应该切成好几行，实际 {} 行", rows.len());
        for (i, row) in rows.iter().enumerate() {
            let limit = if i == 0 { first } else { rest };
            let width = measure_text(&font, 20.0, row);
            assert!(width <= limit, "第 {i} 行宽 {width} 超了 {limit}: {row}");
        }
        // 一个字都没丢
        let got: String = rows.concat().split_whitespace().collect();
        let want: String = text.split_whitespace().collect();
        assert_eq!(got, want);
        // 标点不会跑到行首
        for (i, row) in rows.iter().enumerate().skip(1) {
            assert!(
                !row.chars().next().is_some_and(cannot_start_line),
                "第 {i} 行拿标点在开头: {row}"
            );
        }
        // 拉丁词没有被从中间劈开（不会出现半截的 "wra"）
        assert!(
            rows.iter().all(|row| !row.contains("wra") || row.contains("wrap")),
            "拉丁词被劈开了: {rows:?}"
        );
    }

    #[test]
    fn long_danmaku_takes_more_rows() {
        let renderer = renderer();
        let painted = |text: &str| {
            let pixels = renderer.render(420, 560, &[line(text)], 0.0);
            pixels.as_chunks::<4>().0.iter().filter(|px| px[3] > 0).count()
        };
        let short = painted("短的一条");
        let long = painted(
            "这条弹幕特别长特别长特别长特别长特别长特别长特别长特别长特别长特别长特别长特别长特别长特别长特别长特别长特别长特别长特别长特别长特别长特别长特别长特别长",
        );
        assert!(
            long > short * 2,
            "长弹幕应该换行成好几行：短 {short} 像素，长 {long} 像素"
        );
    }

    #[test]
    fn empty_canvas_is_transparent() {
        let renderer = renderer();
        let pixels = renderer.render(120, 80, &[], 0.0);
        assert_eq!(pixels.len(), 120 * 80 * 4);
        // 全零 = 全透明：没弹幕时这一层什么都不显示
        assert!(pixels.iter().all(|byte| *byte == 0));
    }

    fn line(text: &str) -> DrawLine {
        DrawLine {
            kind: Kind::Danmaku,
            prefix: "测试: ".into(),
            text: text.into(),
            alpha: 1.0,
            enter: 1.0,
            collapse: 1.0,
        }
    }

    #[test]
    fn panel_is_full_width_and_hugs_content() {
        let renderer = renderer();
        let (width, height) = (200usize, 200usize);
        let pixels = renderer.render(width as u32, height as u32, &[line("你好")], 0.0);
        let alpha_at = |x: usize, y: usize| pixels[(y * width + x) * 4 + 3];
        // 顶部是空的（底板只裹住这一行）
        assert!((0..width).all(|x| alpha_at(x, 0) == 0), "顶部应该透明");
        // 底部整宽都是底板（方形、铺满宽度），左边留白也应该是暗底
        let bottom = height - 1;
        assert!((0..width).all(|x| alpha_at(x, bottom) > 0), "底板要铺满宽度");
        // 底板是暗的：预乘后各通道都不高
        let px = &pixels[(bottom * width) * 4..(bottom * width) * 4 + 4];
        assert!(px[0] < 40 && px[1] < 40 && px[2] < 40, "底板应该是暗色");
    }

    #[test]
    fn more_lines_make_taller_panel() {
        let renderer = renderer();
        let fill = |pixels: &[u8], width: usize| {
            (0..200)
                .filter(|y| pixels[(y * width) * 4 + 3] > 0)
                .count()
        };
        let one = renderer.render(200, 200, &[line("一")], 0.0);
        let three = renderer.render(200, 200, &[line("一"), line("二"), line("三")], 0.0);
        assert!(fill(&three, 200) > fill(&one, 200), "弹幕多了底板应该变高");
    }

    #[test]
    fn scroll_shifts_content_down() {
        let renderer = renderer();
        let (width, height) = (200usize, 220usize);
        let top_of_panel = |scroll: f32| {
            let pixels = renderer.render(width as u32, height as u32, &[line("你好")], scroll);
            (0..height)
                .find(|y| pixels[(y * width) * 4 + 3] > 0)
                .expect("应该画了东西")
        };
        let settled = top_of_panel(0.0);
        let shifted = top_of_panel(10.0);
        assert_eq!(shifted - settled, 10, "scroll 应该把整块内容往下推");
        // 推出去的部分要落到画布外（底部裁掉），不能反过来往上跑
        assert!(shifted > settled);
    }

    #[test]
    fn fading_line_gets_fainter() {
        let renderer = renderer();
        let line = |alpha: f32| {
            vec![DrawLine {
                kind: Kind::Danmaku,
                prefix: String::new(),
                text: "淡出".into(),
                alpha,
                enter: 1.0,
                collapse: 1.0,
            }]
        };
        let peak = |pixels: Vec<u8>| {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|px| px[3] as u32)
                .max()
                .unwrap()
        };
        let solid = peak(renderer.render(120, 60, &line(1.0), 0.0));
        let half = peak(renderer.render(120, 60, &line(0.5), 0.0));
        assert!(solid > half, "透明度降低后像素应该更淡");
        assert!(half > 0);
    }

    #[test]
    fn a_new_line_slides_up_from_below() {
        let renderer = renderer();
        let line = |enter: f32| DrawLine {
            kind: Kind::Danmaku,
            prefix: "小明: ".to_string(),
            text: "新弹幕从下面滑上来".to_string(),
            alpha: 1.0,
            enter,
                collapse: 1.0,
            };
        // 还没入场：这条基本贴着画布下沿（大部分被裁掉），亮字比落位时少很多
        let bright = |pixels: &[u8]| {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|px| px[0] > 200 && px[1] > 200 && px[2] > 200)
                .count()
        };
        let arriving = renderer.render(320, 120, &[line(0.0)], 0.0);
        let settled = renderer.render(320, 120, &[line(1.0)], 0.0);
        assert!(
            bright(&arriving) * 3 < bright(&settled),
            "入场中应该被裁掉大半：{} vs {}",
            bright(&arriving),
            bright(&settled)
        );
        // 老弹幕（enter = 1）不受任何影响
        assert_eq!(renderer.render(320, 120, &[line(1.0)], 0.0), settled);
    }

    #[test]
    fn custom_colors_show_up() {
        let mut renderer = renderer();
        renderer.theme.name_color = (255, 0, 0);
        renderer.theme.text_color = (0, 255, 0);
        let pixels = renderer.render(
            200,
            80,
            &[DrawLine {
                kind: Kind::Danmaku,
                prefix: "a:".into(),
                text: "b".into(),
                alpha: 1.0,
                enter: 1.0,
                collapse: 1.0,
            }],
            0.0,
        );
        // 缓冲是 BGRA：[2]=红、[1]=绿
        let chunks = pixels.as_chunks::<4>().0;
        assert!(chunks.iter().any(|px| px[2] > 200 && px[1] < 80), "昵称没变红");
        assert!(chunks.iter().any(|px| px[1] > 200 && px[2] < 80), "正文没变绿");
    }

    #[test]
    fn emoji_gets_rendered_in_color() {
        let file = std::fs::File::open("/usr/share/fonts/noto/NotoColorEmoji.ttf")
            .expect("测试要装 noto-fonts-emoji");
        let map = unsafe { memmap2::Mmap::map(&file) }.unwrap();
        let mut renderer = renderer();
        renderer.set_emoji_font(map, 0);
        let pixels = renderer.render(
            200,
            120,
            &[DrawLine {
                kind: Kind::Danmaku,
                prefix: String::new(),
                text: "😀".into(),
                alpha: 1.0,
                enter: 1.0,
                collapse: 1.0,
            }],
            0.0,
        );
        // 😀 是黄的：缓冲是 BGRA，所以 b 明显低于 r、g
        let yellow = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[3] > 200 && px[2] > 150 && px[1] > 130 && px[0] < 120)
            .count();
        assert!(yellow > 20, "emoji 没画出颜色（找到 {yellow} 个黄色像素）");
    }

    #[test]
    fn output_is_bgra_so_blue_stays_blue() {
        // wl_shm 的 Argb8888 在小端机器上内存里是 [B,G,R,A]，而 tiny-skia 画出来是
        // [R,G,B,A]；这里盯住前缀色（蓝 132,170,255）在缓冲里的字节顺序。
        let mut renderer = renderer();
        renderer.theme.name_color = (132, 170, 255);
        let lines = vec![DrawLine {
            kind: Kind::Danmaku,
            prefix: "蓝: ".into(),
            text: String::new(),
            alpha: 1.0,
            enter: 1.0,
                collapse: 1.0,
            }];
        let pixels = renderer.render(160, 60, &lines, 0.0);
        let count = |want: [u8; 3]| {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|px| {
                    px[0].abs_diff(want[0]) < 24
                        && px[1].abs_diff(want[1]) < 24
                        && px[2].abs_diff(want[2]) < 24
                        && px[3] > 200
                })
                .count()
        };
        // 蓝色 (132,170,255) 落到内存里应该是 [255,170,132]
        let correct = count([255, 170, 132]);
        let swapped = count([132, 170, 255]);
        assert!(correct > 0, "没找到前缀色像素");
        assert!(
            correct > swapped,
            "R/B 还是反的：正确 {correct} 个，反的 {swapped} 个"
        );
    }
}
