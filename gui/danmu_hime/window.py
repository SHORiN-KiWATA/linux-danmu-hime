"""设置窗口（照哔哩哔哩弹幕姬那套来：字号、位置这类都用滑块，不让人填数字）。

改哪一项就往 config.json 写一次（400ms 防抖），浮层自己会热重载；
只有房间/cookie/字体/显示器这几项要重启浮层，保存后会弹一条提示。
"""

from __future__ import annotations

import shutil
import subprocess
import threading

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
gi.require_version("Pango", "1.0")
gi.require_version("PangoCairo", "1.0")
from gi.repository import Adw, Gdk, GLib, Gtk, Pango, PangoCairo  # noqa: E402

from . import config, login, service
from .i18n import _  # noqa: E402

SAVE_DELAY_MS = 400

# 预览里用的示例弹幕（尽量跟真实的样子像：昵称 + 内容，带 emoji、带长句）
# 预览里的样本：kind 跟浮层那边的 Kind 对应，gift 行正文用礼物色
SAMPLES = [
    ("danmaku", "晴晴了", "近亲走过去吃大"),
    ("danmaku", "[瑞乃酱·6]", "这条全是 emoji：😀😂🔥✨🎉💯"),
    ("danmaku", "毒树之果", "下路让你轮上了，中文随便断但英文 words must not be split"),
]
# 礼物行：前缀是「昵称 动作」，正文是「礼物名 ×N」
GIFT_SAMPLE = ("gift", "xiaop668 投喂", "星星之火 ×1")

ANCHORS = [
    ("top-left", "↖"),
    ("top", "↑"),
    ("top-right", "↗"),
    ("left", "←"),
    ("center", "•"),
    ("right", "→"),
    ("bottom-left", "↙"),
    ("bottom", "↓"),
    ("bottom-right", "↘"),
]

LAYERS = [
    ("top", "layer-short-top"),
    ("overlay", "layer-short-overlay"),
    ("bottom", "layer-short-bottom"),
    ("background", "layer-short-background"),
]

# 浮层里昵称是淡蓝色、内容是近白色（跟 render.rs 保持一致）
NAME_COLOR = (0.58, 0.72, 0.97)
TEXT_COLOR = (0.93, 0.95, 0.98)


def _to_rgba(hex_text: str) -> "Gdk.RGBA":
    rgba = Gdk.RGBA()
    if not rgba.parse(hex_text or ""):
        rgba.parse("#ffffff")
    return rgba


def _to_hex(rgba: "Gdk.RGBA") -> str:
    return "#{:02x}{:02x}{:02x}".format(
        round(rgba.red * 255), round(rgba.green * 255), round(rgba.blue * 255)
    )


def _rgb(hex_text: str) -> tuple[float, float, float]:
    rgba = _to_rgba(hex_text)
    return (rgba.red, rgba.green, rgba.blue)


def _pango_font(path: str | None, size_px: float) -> Pango.FontDescription:
    """字体文件 → Pango 描述，给预览用；查不到就用系统默认。"""
    desc = Pango.FontDescription()
    family = None
    if path and shutil.which("fc-scan"):
        try:
            proc = subprocess.run(
                ["fc-scan", "-f", "%{family}", path], capture_output=True, text=True, timeout=5
            )
            family = proc.stdout.split(",")[0].strip() or None
        except (OSError, subprocess.TimeoutExpired):  # pragma: no cover - 环境问题
            family = None
    if family:
        desc.set_family(family)
    desc.set_absolute_size(size_px * Pango.SCALE)
    return desc


class WidgetRow(Adw.PreferencesRow):
    """把任意控件当成一行塞进 PreferencesGroup。

    Adw 1.9 的 PreferencesGroup 只认 AdwPreferencesRow，直接 add 一个 Grid/DrawingArea
    会被丢到卡片外面去。
    """

    def __init__(self, child: Gtk.Widget, margin: int = 12) -> None:
        super().__init__()
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        for side in ("top", "bottom", "start", "end"):
            getattr(box, f"set_margin_{side}")(margin)
        box.append(child)
        self.set_child(box)
        self.set_activatable(False)


class PanelPreview(Gtk.DrawingArea):
    """照着浮层的排法画一遍：宽度、字号、行距、透明度、换行都是真的。"""

    def __init__(self, window: "ConfigWindow") -> None:
        super().__init__()
        self.window = window
        self.set_size_request(-1, 320)
        self.set_draw_func(self._draw)

    def _draw(self, _area, cr, width: int, height: int) -> None:
        values = self.window.values
        zoom = float(values.get("zoom") or 1.0)
        font_size = max(6.0, float(values["font_size"]) * zoom)
        # 预览里一行一条消息，所以间距看 row_gap（消息之间），不是 line_gap
        gap = float(values.get("row_gap", 10.0)) * zoom
        opacity = max(0.0, min(1.0, float(values["opacity"])))
        panel_rgb = _rgb(values.get("panel_color") or config.DEFAULTS["panel_color"])
        name_rgb = _rgb(values.get("name_color") or config.DEFAULTS["name_color"])
        text_rgb = _rgb(values.get("text_color") or config.DEFAULTS["text_color"])
        gift_rgb = _rgb(values.get("gift_color") or config.DEFAULTS["gift_color"])

        cr.set_source_rgb(0.10, 0.11, 0.13)
        cr.paint()

        # 浮层多大就画多大；放不下就整体等比缩（字号一起缩）
        panel_width = max(80.0, float(values["width"]))
        panel_limit = max(60.0, float(values["height"]))
        del panel_limit  # 只按宽度收一收，高度不缩：预览要 1:1 才看得出字号行距
        scale = min(1.0, (width - 12) / panel_width)
        size = font_size * scale
        gap_px = gap * scale
        pad_x, pad_y = size * 0.7, size * 0.3
        inner = max(size, panel_width * scale - pad_x * 2)
        desc = _pango_font(values.get("font"), size)

        # 排一遍：最新的贴底，往上一行一行码（跟浮层一样）
        rows = []
        used = pad_y * 2
        for kind, name, text in reversed(SAMPLES + [GIFT_SAMPLE]):
            prefix = self.create_pango_layout(f"{name}: ")
            prefix.set_font_description(desc)
            prefix_width = prefix.get_pixel_size()[0]
            body = self.create_pango_layout(text)
            body.set_font_description(desc)
            body.set_wrap(Pango.WrapMode.WORD_CHAR)  # 英文整词不拆，汉字随便断
            body.set_width(int(max(size, inner - prefix_width)) * Pango.SCALE)
            body.set_spacing(int(gap_px * Pango.SCALE))
            body_height = body.get_pixel_size()[1]
            rows.append((kind, prefix, prefix_width, body, body_height))
            used += body_height + gap_px
        used -= gap_px
        panel_height = max(size, used)

        left = max(6.0, width - panel_width * scale - 6)
        top = max(0.0, height - panel_height - 6)

        # 底板 + 内容都裁在底板里
        cr.save()
        cr.rectangle(left, top, panel_width * scale, panel_height)
        cr.clip()
        cr.set_source_rgba(panel_rgb[0], panel_rgb[1], panel_rgb[2], 0.55 * opacity)
        cr.paint()
        cr.restore()

        cr.save()
        cr.rectangle(left, top, panel_width * scale, panel_height)
        cr.clip()
        y = top + pad_y
        for kind, prefix, prefix_width, body, body_height in rows:
            # 礼物行的昵称也是昵称色，只有正文换成礼物色（和浮层一致）
            cr.set_source_rgba(name_rgb[0], name_rgb[1], name_rgb[2], 1.0)
            cr.move_to(left + pad_x, y)
            PangoCairo.show_layout(cr, prefix)
            body_rgb = gift_rgb if kind == "gift" else text_rgb
            cr.set_source_rgba(body_rgb[0], body_rgb[1], body_rgb[2], 1.0)
            cr.move_to(left + pad_x + prefix_width, y)
            PangoCairo.show_layout(cr, body)
            y += body_height + gap_px
        cr.restore()


class ScreenSketch(Gtk.DrawingArea):
    """按真实显示器比例画一块屏，浮层那块可以直接拖。"""

    PAD = 12.0

    def __init__(self, window: "ConfigWindow") -> None:
        super().__init__()
        self.window = window
        self.set_size_request(-1, 320)
        self.set_draw_func(self._draw)
        self._drag = (0.0, 0.0)
        self._redraw_id = 0
        try:
            self._cursor_grab = Gdk.Cursor.new_from_name("grabbing", None)
            self.set_cursor(Gdk.Cursor.new_from_name("grab", None))
        except Exception:  # pragma: no cover - 没有主题就拉倒
            self._cursor_grab = None
        drag = Gtk.GestureDrag()
        drag.connect("drag-begin", self._on_drag_begin)
        drag.connect("drag-update", self._on_drag_update)
        drag.connect("drag-end", self._on_drag_end)
        self.add_controller(drag)

    # 显示器在示意图里占的位置，以及「屏幕像素 → 示意图像素」的缩放
    def _screen(self, width: int, height: int) -> tuple[float, float, float, float, float]:
        screen_w, screen_h = max(1.0, self.window.screen_size[0]), max(1.0, self.window.screen_size[1])
        pad = self.PAD
        scale = min((width - pad * 2) / screen_w, (height - pad * 2) / screen_h)
        box_w, box_h = screen_w * scale, screen_h * scale
        return (width - box_w) / 2, (height - box_h) / 2, box_w, box_h, scale

    # 浮层窗口（W×H）：合成器只认贴边的那几个 margin，所以这里也按贴边算
    def _surface(
        self, width: int, height: int, drag: tuple[float, float] = (0.0, 0.0)
    ) -> tuple[float, float, float, float, float]:
        values = self.window.values
        sx, sy, sw, sh, scale = self._screen(width, height)
        surface_w = min(sw, max(8.0, float(values["width"]) * scale))
        surface_h = min(sh, max(8.0, float(values["height"]) * scale))
        margin = min(float(values["margin"]) * scale, min(sw, sh) / 2 - 3)
        offset_x = float(values.get("offset_x") or 0) * scale
        offset_y = float(values.get("offset_y") or 0) * scale
        anchor = values["anchor"]
        # 正数 = 离贴着的那条边更远：贴右边时正数往左，贴底时正数往上
        x = sx + margin + offset_x if "left" in anchor else (
            sx + sw - margin - offset_x - surface_w if "right" in anchor else sx + (sw - surface_w) / 2
        )
        y = sy + margin + offset_y if "top" in anchor else (
            sy + sh - margin - offset_y - surface_h if "bottom" in anchor else sy + (sh - surface_h) / 2
        )
        x += drag[0]
        y += drag[1]
        x = min(max(x, sx), max(sx, sx + sw - surface_w))
        y = min(max(y, sy), max(sy, sy + sh - surface_h))
        return x, y, surface_w, surface_h, scale

    # 底板：占满窗口宽度，贴在窗口底边（新弹幕就是从下沿挤进来的）
    def _box(
        self, width: int, height: int, drag: tuple[float, float] = (0.0, 0.0)
    ) -> tuple[float, float, float, float, float]:
        values = self.window.values
        x, y, surface_w, surface_h, scale = self._surface(width, height, drag)
        zoom = float(values.get("zoom") or 1.0)
        line_h = float(values["font_size"]) * 1.15 * zoom * scale
        panel_h = min(surface_h, max(10.0, line_h * 3.0))
        return x, y + surface_h - panel_h, surface_w, panel_h, scale

    def _draw(self, _area, cr, width: int, height: int) -> None:
        if width < 40 or height < 40:
            return
        cr.set_source_rgb(0.13, 0.14, 0.17)
        cr.paint()
        sx, sy, sw, sh, scale = self._screen(width, height)

        # 显示器
        cr.set_source_rgb(0.09, 0.10, 0.12)
        cr.rectangle(sx, sy, sw, sh)
        cr.fill()
        cr.set_source_rgba(0.45, 0.48, 0.55, 0.7)
        cr.set_line_width(1.0)
        cr.rectangle(sx + 0.5, sy + 0.5, sw - 1, sh - 1)
        cr.stroke()

        # 浮层窗口：虚框（它比底板高得多，底板贴在它底边）
        fx, fy, fw, fh, _ = self._surface(width, height, self._drag)
        cr.set_source_rgba(0.55, 0.60, 0.70, 0.55)
        cr.set_line_width(1.0)
        cr.set_dash([4.0, 4.0])
        cr.rectangle(fx + 0.5, fy + 0.5, max(1.0, fw - 1), max(1.0, fh - 1))
        cr.stroke()
        cr.set_dash([])

        # 弹幕底板：实心，能拖的就是它（拖动中的偏移必须带上，不然不跟手）
        x, y, panel_w, panel_h, _ = self._box(width, height, self._drag)
        cr.set_source_rgba(0.35, 0.62, 1.0, 0.35)
        cr.rectangle(x, y, panel_w, panel_h)
        cr.fill()
        cr.set_source_rgba(0.55, 0.75, 1.0, 0.9)
        cr.rectangle(x + 0.5, y + 0.5, max(1.0, panel_w - 1), max(1.0, panel_h - 1))
        cr.stroke()

        # 屏中间画两条参考线，好对中
        cr.set_source_rgba(0.45, 0.48, 0.55, 0.25)
        cr.set_line_width(1.0)
        cr.move_to(sx + sw / 2, sy)
        cr.line_to(sx + sw / 2, sy + sh)
        cr.move_to(sx, sy + sh / 2)
        cr.line_to(sx + sw, sy + sh / 2)
        cr.stroke()

    def _on_drag_begin(self, _gesture, _x: float, _y: float) -> None:
        self._drag = (0.0, 0.0)
        if self._cursor_grab is not None:
            self.set_cursor(self._cursor_grab)

    def _on_drag_update(self, _gesture, off_x: float, off_y: float) -> None:
        if (off_x, off_y) == self._drag:
            return
        self._drag = (off_x, off_y)
        # 鼠标一个事件一个事件地来，重画合并到 ~60fps，不然一张卡上重绘很卡
        if not self._redraw_id:
            self._redraw_id = GLib.timeout_add(16, self._redraw)

    def _redraw(self) -> bool:
        self._redraw_id = 0
        self.queue_draw()
        return False

    def _on_drag_end(self, _gesture, off_x: float, off_y: float) -> None:
        """松手就把方块落在哪儿翻译成「贴哪条边 + 上下左右微调」。"""
        if self._redraw_id:
            GLib.source_remove(self._redraw_id)
            self._redraw_id = 0
        width, height = self.get_width(), self.get_height()
        self._drag = (0.0, 0.0)
        try:
            self.set_cursor(Gdk.Cursor.new_from_name("grab", None))
        except Exception:  # pragma: no cover
            pass
        if width <= 0 or height <= 0:
            return
        sx, sy, sw, sh, scale = self._screen(width, height)
        scale = max(scale, 1e-3)
        x, y, panel_w, panel_h, _ = self._box(width, height, (off_x, off_y))

        # 只贴四个角：落点在哪半边就用哪个角，中间方位合成器会忽略 margin，
        # 怎么拖都不动（就是之前"离屏幕那么远却贴边"的原因）。
        col = "left" if x + panel_w / 2 < sx + sw / 2 else "right"
        row = "top" if y + panel_h / 2 < sy + sh / 2 else "bottom"
        anchor = f"{row}-{col}"

        values = self.window.values
        margin = float(values["margin"]) * scale
        surface_w = min(sw, max(8.0, float(values["width"]) * scale))
        surface_h = min(sh, max(8.0, float(values["height"]) * scale))
        # 底板贴在窗口底边，所以反解时要先换成窗口的位置
        surface_x = sx + margin if col == "left" else sx + sw - margin - surface_w
        surface_y = sy + margin if row == "top" else sy + sh - margin - surface_h
        natural_x = surface_x                       # 底板左边 = 窗口左边
        natural_y = surface_y + surface_h - panel_h  # 底板贴窗口底边

        def snap(pixels: float) -> int:
            step = int(round(round(pixels / scale) / 2.0) * 2)  # 2px 一格
            return max(-3000, min(3000, step))

        # 贴右边/贴底时，正数偏移是把窗口往屏幕里推（跟 overlay 的语义一致）
        offset_x = (natural_x - x) if col == "right" else (x - natural_x)
        offset_y = (natural_y - y) if row == "bottom" else (y - natural_y)
        self.window.apply_position(anchor, snap(offset_x), snap(offset_y), redraw=False)


class LoginDialog(Adw.Dialog):
    """扫码登录：显示二维码，每两秒问一次扫没扫。"""

    def __init__(self, window: "ConfigWindow") -> None:
        super().__init__(title=_("Scan to log in"), content_width=300)
        self.window = window
        self.key = ""
        self.timer = 0
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
        for side in ("top", "bottom", "start", "end"):
            getattr(box, f"set_margin_{side}")(18)
        self.picture = Gtk.Picture()
        self.picture.set_size_request(240, 240)
        box.append(self.picture)
        self.status = Gtk.Label(label=_("Loading QR"))
        self.status.set_wrap(True)
        self.status.set_justify(Gtk.Justification.CENTER)
        self.status.add_css_class("dim-label")
        box.append(self.status)
        self.refresh = Gtk.Button(label=_("Refresh QR"))
        self.refresh.set_visible(False)
        self.refresh.connect("clicked", lambda *_: self.start())
        box.append(self.refresh)
        self.set_child(box)
        self.connect("closed", lambda *_: self._stop())

    def start(self) -> None:
        self.refresh.set_visible(False)
        self.picture.set_visible(True)
        self.status.set_text(_("Loading QR"))
        threading.Thread(target=self._generate, daemon=True).start()

    def _stop(self) -> None:
        if self.timer:
            GLib.source_remove(self.timer)
            self.timer = 0

    def _generate(self) -> None:
        try:
            url, key = login.generate()
        except (OSError, ValueError) as err:
            GLib.idle_add(self._failed, str(err))
            return
        GLib.idle_add(self._show, key, login.qr_png(url), url)

    def _show(self, key: str, png: bytes | None, url: str) -> bool:
        self.key = key
        if png:
            self.picture.set_paintable(Gdk.Texture.new_from_bytes(GLib.Bytes.new(png)))
        else:
            self.picture.set_visible(False)
        self.status.set_text(_("Scan hint") if png else url)
        if self.timer:
            GLib.source_remove(self.timer)
        self.timer = GLib.timeout_add_seconds(2, self._tick)
        return False

    def _tick(self) -> bool:
        threading.Thread(target=self._poll, daemon=True).start()
        return True

    def _poll(self) -> None:
        try:
            state, cookie = login.poll(self.key)
        except OSError:
            return
        GLib.idle_add(self._state, state, cookie)

    def _state(self, state: str, cookie: str | None) -> bool:
        if state == "waiting":
            return False
        if state == "scanned":
            self.status.set_text(_("Scanned, confirm on your phone"))
            return False
        self._stop()
        if state == "expired":
            self.status.set_text(_("QR expired"))
            self.refresh.set_visible(True)
        elif cookie:
            self.window.set_cookie(cookie)
            self.close()
        else:
            self.status.set_text(_("Login failed, try again"))
            self.refresh.set_visible(True)
        return False

    def _failed(self, message: str) -> bool:
        self._stop()
        self.picture.set_visible(False)
        self.status.set_text(message)
        self.refresh.set_visible(True)
        return False


class ConfigWindow(Adw.ApplicationWindow):
    def __init__(self, app: Adw.Application) -> None:
        super().__init__(application=app, title=_("Danmaku Overlay"))
        self.set_default_size(780, 740)
        self.set_size_request(560, 480)
        self.values = config.load()
        self._saved = dict(self.values)
        self.outputs = self._detect_outputs()
        self.screen_size = self._screen_size()
        self._loading = True
        self._save_timer = 0
        self._sliders: dict[str, Gtk.Scale] = {}
        self._toast = Adw.ToastOverlay()
        self._build_ui()
        self._loading = False
        self._sync_sliders()
        self.refresh_status()
        GLib.timeout_add_seconds(4, self._poll_status)

    # ---------- 界面 ----------

    def _build_ui(self) -> None:
        # 两页：第一页进来看得到房间号和开关，第二页管外观。
        # 用 Gtk.StackSwitcher 而不是 Adw.ViewSwitcher——后者的宽模式会在标题旁边塞图标。
        self.stack = Gtk.Stack()
        self.stack.add_titled(self._page_main(), "main", _("Danmaku"))
        self.stack.add_titled(self._page_appearance(), "appearance", _("Appearance"))
        self.stack.set_visible_child_name("main")

        switcher = Gtk.StackSwitcher()
        switcher.set_stack(self.stack)
        header = Adw.HeaderBar()
        header.set_title_widget(switcher)

        self.status_label = Gtk.Label()
        self.status_label.add_css_class("dim-label")
        self.status_label.add_css_class("caption")
        self.status_icon = Gtk.Image.new_from_icon_name("media-record-symbolic")
        status_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        status_box.append(self.status_icon)
        status_box.append(self.status_label)
        header.pack_end(status_box)

        toolbar = Adw.ToolbarView()
        toolbar.add_top_bar(header)
        toolbar.set_content(self.stack)
        self._toast.set_child(toolbar)
        self.set_content(self._toast)

    # ---------- 第一页：房间号 / 开关 / 位置 / 节奏 ----------

    def _page_main(self) -> Gtk.Widget:
        page = Adw.PreferencesPage()

        login_group = Adw.PreferencesGroup(title=_("Login"))
        self.login_row = Adw.ActionRow(title=_("Login"))
        self.login_badge = Gtk.Label()
        self.login_badge.add_css_class("dim-label")
        self.login_row.add_suffix(self.login_badge)
        self.login_button = Gtk.Button(label=_("Scan to log in"))
        self.login_button.add_css_class("suggested-action")
        self.login_button.set_valign(Gtk.Align.CENTER)
        self.login_button.connect("clicked", lambda *_: self._open_login())
        self.login_row.add_suffix(self.login_button)
        login_group.add(self.login_row)
        # 不要把 SESSDATA 明文摆在界面上：这一行留空就表示「用现在这个」，
        # 要换账号直接粘一条新的进来（空着不会写回配置，不会把已登录的擦掉）。
        self.cookie_row = Adw.EntryRow(title=_("Cookie (optional)"))
        self.cookie_row.set_text("")
        self.cookie_row.set_tooltip_text(_("Cookie hidden"))
        self.cookie_row.connect("changed", self._on_cookie_changed)
        login_group.add(self.cookie_row)
        self.login_hint = Adw.ActionRow(title=_("Login hint"))
        self.login_hint.add_css_class("property")
        login_group.add(self.login_hint)
        page.add(login_group)

        room_group = Adw.PreferencesGroup(title=_("Live room"))
        self.room_row = self._entry_row(room_group, "room", _("Room"), "")
        self.room_row.set_tooltip_text(_("Room hint"))

        buttons = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        buttons.set_valign(Gtk.Align.CENTER)
        self.start_button = Gtk.Button(icon_name="media-playback-start-symbolic")
        self.start_button.set_tooltip_text(_("Start the overlay"))
        self.start_button.connect("clicked", lambda *_: self._service_action("start"))
        self.stop_button = Gtk.Button(icon_name="media-playback-stop-symbolic")
        self.stop_button.set_tooltip_text(_("Stop the overlay"))
        self.stop_button.connect("clicked", lambda *_: self._service_action("stop"))
        self.restart_button = Gtk.Button(icon_name="view-refresh-symbolic")
        self.restart_button.set_tooltip_text(_("Restart the overlay"))
        self.restart_button.add_css_class("suggested-action")
        self.restart_button.connect("clicked", lambda *_: self._service_action("restart"))
        self.test_button = Gtk.Button(icon_name="mail-send-symbolic")
        self.test_button.set_tooltip_text(_("Test danmaku"))  # 本地模拟，不会发到直播间
        self.test_button.connect("clicked", lambda *_: self._send_test_danmaku())
        for button in (self.start_button, self.stop_button, self.restart_button, self.test_button):
            buttons.append(button)

        self.process_row = Adw.ActionRow(title=_("Overlay process"))
        self.process_row.add_suffix(buttons)
        room_group.add(self.process_row)

        self.autostart_row = Adw.SwitchRow(title=_("Start at login"))
        self.autostart_row.connect("notify::active", self._on_autostart_toggled)
        room_group.add(self.autostart_row)
        page.add(room_group)

        anchor_group = Adw.PreferencesGroup(title=_("Position settings"))
        anchor_group.set_description(_("Position hint"))
        self.anchor_buttons: dict[str, Gtk.ToggleButton] = {}
        self.sketch = ScreenSketch(self)
        self.sketch.set_hexpand(True)
        anchor_group.add(WidgetRow(self.sketch, margin=10))

        outputs = list(self.outputs)
        for name in service.list_outputs():
            if name not in outputs:
                outputs.append(name)
        current = self.values.get("output")
        if current and current not in outputs:
            outputs.append(current)
        self.output_items = [_("Auto (compositor)")] + outputs
        self.output_row = Adw.ComboRow(title=_("Display"))
        self.output_row.set_subtitle(_("Display hint"))
        self.output_row.set_model(Gtk.StringList.new(self.output_items))
        self.output_row.set_selected(
            0 if not current else max(0, self.output_items.index(current))
        )
        self.output_row.connect("notify::selected", self._on_output_selected)
        anchor_group.add(self.output_row)
        # 「缩放」是整体放大倍数：字和行距一起放大。
        # 之前这个滑块写的是设备像素比，设成 50% 反而让合成器把画面放大，字看着更大。
        self._add_slider(
            anchor_group, "zoom", _("Scale"), 50, 300, 5, "%", subtitle=_("Scale hint")
        )
        page.add(anchor_group)

        timing_group = Adw.PreferencesGroup(title=_("Timing"))
        self._add_slider(timing_group, "ttl", _("Time to live"), 2, 90, 1, "s")
        self._add_slider(timing_group, "fade", _("Fade"), 0, 5, 0.1, "s", digits=1)
        self._add_slider(timing_group, "max_lines", _("Max lines"), 10, 500, 10, "lines")
        page.add(timing_group)
        return page

    # ---------- 第二页：外观 ----------

    def _page_appearance(self) -> Gtk.Widget:
        page = Adw.PreferencesPage()

        preview_group = Adw.PreferencesGroup(title=_("Preview"))
        preview_group.set_description(_("Preview hint"))
        self._preview = PanelPreview(self)
        preview_group.add(WidgetRow(self._preview, margin=10))
        page.add(preview_group)

        font_group = Adw.PreferencesGroup(title=_("Appearance"))
        self.font_row = Adw.ActionRow(title=_("Font"), subtitle=_("Font hint"))
        self.font_button = Gtk.FontDialogButton(dialog=Gtk.FontDialog())
        self.font_button.set_use_font(True)
        self.font_button.set_use_size(False)
        self.font_button.add_css_class("flat")
        self.font_button.set_valign(Gtk.Align.CENTER)
        self.font_button.connect("notify::font-desc", self._on_font_chosen)
        if self.values.get("font"):
            self.font_button.set_font_desc(_pango_font(self.values["font"], 12.0))
        self.font_row.add_suffix(self.font_button)
        font_group.add(self.font_row)
        self.font_file_row = self._entry_row(
            font_group, "font", _("Font file"), on_change=self._on_font_file_changed
        )
        self.emoji_row = self._entry_row(font_group, "emoji_font", _("Emoji font file"))
        self.gift_row = Adw.SwitchRow(title=_("Show gifts"), subtitle=_("Show gifts hint"))
        self.gift_row.set_active(bool(self.values.get("gift", True)))
        self.gift_row.connect(
            "notify::active",
            lambda row, _p: None if self._loading else self._set("gift", row.get_active()),
        )
        font_group.add(self.gift_row)
        self.gift_icon_row = Adw.SwitchRow(
            title=_("Show gift icons"), subtitle=_("Show gift icons hint")
        )
        self.gift_icon_row.set_active(bool(self.values.get("gift_icon", True)))
        self.gift_icon_row.connect(
            "notify::active",
            lambda row, _p: None if self._loading else self._set("gift_icon", row.get_active()),
        )
        font_group.add(self.gift_icon_row)
        self.medal_row = Adw.SwitchRow(title=_("Show medal"), subtitle=_("Show medal hint"))
        self.medal_row.set_active(bool(self.values.get("medal", True)))
        self.medal_row.connect("notify::active", self._on_medal_toggled)
        font_group.add(self.medal_row)
        self.avatar_row = Adw.SwitchRow(title=_("Show avatars"), subtitle=_("Show avatars hint"))
        self.avatar_row.set_active(bool(self.values.get("avatar", True)))
        self.avatar_row.connect(
            "notify::active",
            lambda row, _p: None if self._loading else self._set("avatar", row.get_active()),
        )
        font_group.add(self.avatar_row)
        self.avatar_round_row = Adw.SwitchRow(
            title=_("Round avatars"), subtitle=_("Round avatars hint")
        )
        self.avatar_round_row.set_active(bool(self.values.get("avatar_round", True)))
        self.avatar_round_row.connect(
            "notify::active",
            lambda row, _p: None if self._loading else self._set("avatar_round", row.get_active()),
        )
        font_group.add(self.avatar_round_row)
        self._add_slider(font_group, "font_size", _("Font size"), 10, 48, 1, "px")
        self._add_slider(
            font_group, "row_gap", _("Row gap"), 0, 40, 1, "px", subtitle=_("Row gap hint")
        )
        self._add_slider(
            font_group, "line_gap", _("Line gap"), 0, 24, 1, "px", subtitle=_("Line gap hint")
        )
        self._add_slider(
            font_group,
            "opacity",
            _("Panel opacity"),
            0,
            100,
            1,
            "%",
            subtitle=_("Panel opacity hint"),
        )
        page.add(font_group)

        color_group = Adw.PreferencesGroup(title=_("Colors"))
        presets = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        presets.set_halign(Gtk.Align.CENTER)
        for preset, (name_hex, text_hex, panel_hex) in config.PRESETS.items():
            button = Gtk.Button(label=preset)
            button.add_css_class("flat")
            button.connect(
                "clicked",
                lambda _b, values=(name_hex, text_hex, panel_hex): self._apply_preset(*values),
            )
            presets.append(button)
        color_group.add(WidgetRow(presets, margin=8))
        self._color_buttons: dict[str, Gtk.ColorDialogButton] = {}
        self._color_row(color_group, "name_color", _("Name color"))
        self._color_row(color_group, "text_color", _("Text color"))
        self._color_row(color_group, "gift_color", _("Gift color"))
        self._color_row(color_group, "panel_color", _("Panel color"))
        page.add(color_group)

        size_group = Adw.PreferencesGroup(title=_("Size"))
        self._add_slider(size_group, "width", _("Width"), 200, 1200, 10, "px")
        self._add_slider(size_group, "height", _("Height"), 120, 2000, 20, "px")
        page.add(size_group)
        return page

    # ---------- 小组件 ----------

    def _entry_row(
        self,
        group: Adw.PreferencesGroup,
        key: str,
        title: str,
        default: str | None = None,
        on_change=None,
    ) -> Adw.EntryRow:
        row = Adw.EntryRow(title=title)
        row.set_text(str(self.values.get(key) or (default or "")))
        row.set_show_apply_button(False)
        if on_change is None:
            row.connect("changed", self._on_text_changed, key)
        else:
            row.connect("changed", on_change)
        group.add(row)
        return row

    def _add_slider(
        self,
        group: Adw.PreferencesGroup,
        key: str,
        title: str,
        lower: float,
        upper: float,
        step: float,
        unit: str,
        subtitle: str | None = None,
        digits: int = 0,
    ) -> Gtk.Scale:
        raw = self.values.get(key)
        if key == "scale":
            initial = 100.0 if raw is None else float(raw) * 100
        elif unit == "%":
            initial = float(raw or 0.0) * 100
        else:
            initial = float(raw or lower)
        row = Adw.ActionRow(title=title)
        if subtitle:
            row.set_subtitle(subtitle)
        label = Gtk.Label()
        label.add_css_class("dim-label")
        label.set_size_request(58, -1)
        label.set_xalign(1.0)
        adjustment = Gtk.Adjustment(
            value=initial,
            lower=lower,
            upper=upper,
            step_increment=step,
            page_increment=step * 5,
        )
        scale = Gtk.Scale(orientation=Gtk.Orientation.HORIZONTAL, adjustment=adjustment)
        scale.set_draw_value(False)
        scale.set_size_request(240, -1)
        scale.set_valign(Gtk.Align.CENTER)
        row.add_suffix(label)
        row.add_suffix(scale)
        row.set_activatable_widget(scale)
        group.add(row)
        self._sliders[key] = scale

        def fmt(number: float) -> str:
            return f"{number:.{digits}f} {unit}".strip()

        def on_change(widget: Gtk.Scale) -> None:
            number = round(widget.get_value(), digits)
            label.set_text(fmt(number))
            if key in config.INT_KEYS:
                self._set(key, int(number))
            elif key == "scale":
                self._set(key, number / 100.0)
            else:
                self._set(key, number / 100.0 if unit == "%" else number)

        label.set_text(fmt(initial))
        scale.connect("value-changed", on_change)
        if key == "scale":
            scale.set_sensitive(False)
        return scale

    # ---------- 改动 ----------

    def _set(self, key: str, value: object) -> None:
        if self._loading or self.values.get(key) == value:
            return
        self.values[key] = value
        self._preview.queue_draw()
        self.sketch.queue_draw()
        self._schedule_save()

    def _schedule_save(self) -> None:
        if self._save_timer:
            GLib.source_remove(self._save_timer)
        self._save_timer = GLib.timeout_add(SAVE_DELAY_MS, self._flush)

    def _flush(self) -> bool:
        self._save_timer = 0
        pending = config.restart_only_changed(self._saved, self.values)
        try:
            config.save(self.values)
        except OSError as err:
            self.toast(_("Could not write the config: {err}").format(err=err))
            return False
        self._saved = dict(self.values)
        if pending:
            # 浮层正在跑就直接重启，别让用户自己去点 ⟳
            if service.unit_exists() and service.is_active():
                self._service_action("restart")
            else:
                self.toast(_("Applied, restart needed"))
        return False

    def _on_text_changed(self, row: Adw.EntryRow, key: str) -> None:
        self._set(key, row.get_text().strip() or None)

    def _on_font_file_changed(self, row: Adw.EntryRow) -> None:
        text = row.get_text().strip()
        self._set("font", text or None)
        self.font_row.set_subtitle(text or _("Font hint"))
        self._preview.queue_draw()

    def _on_font_chosen(self, button: Gtk.FontDialogButton, _param) -> None:
        if self._loading:
            return
        desc = button.get_font_desc()
        family = desc.get_family() if desc else None
        path = None
        if family and shutil.which("fc-match"):
            try:
                proc = subprocess.run(
                    ["fc-match", "-f", "%{file}", family],
                    capture_output=True,
                    text=True,
                    timeout=5,
                )
                path = proc.stdout.strip() or None
            except (OSError, subprocess.TimeoutExpired):  # pragma: no cover
                path = None
        self._loading = True
        self.font_file_row.set_text(path or "")
        self._loading = False
        self._set("font", path)
        self.font_row.set_subtitle(path or _("Font hint"))

    def _open_login(self) -> None:
        dialog = LoginDialog(self)
        self._login_dialog = dialog
        dialog.start()
        dialog.present(self)

    def _on_cookie_changed(self, row: Adw.EntryRow) -> None:
        # 界面上看不到已登录的 cookie，留空就当没改
        if not self.cookie_row.get_text().strip():
            return
        self._set("cookie", row.get_text().strip() or None)
        self._sync_login()

    def set_cookie(self, cookie: str) -> None:
        """扫码成功：写进配置（cookie 是「只能重启生效」的项）。"""
        self._loading = True
        self.cookie_row.set_text(cookie)
        self._loading = False
        self._set("cookie", cookie)
        self._sync_login()
        threading.Thread(target=self._verify_login, args=(cookie,), daemon=True).start()

    def _verify_login(self, cookie: str) -> None:
        uid, name = login.verify(cookie)
        GLib.idle_add(self._after_login, uid, name)

    def _after_login(self, uid: int, name: str) -> bool:
        if name:
            self.login_badge.set_text(_("Logged in as {name}").format(name=name))
        # 浮层正在跑就直接重启一次，省得用户再点一下
        if service.unit_exists() and service.is_active():
            self._service_action("restart")
        else:
            self.toast(_("Logged in, restart the overlay"))
        return False

    def _sync_login(self) -> None:
        state, uid = config.login_state(self.values)
        if state == "logged-in":
            self.login_badge.set_text(
                _("Logged in as {uid}").format(uid=uid) if uid else _("Logged in")
            )
            # 别整行刷绿，低调一点：保持默认灰色
            self.login_badge.remove_css_class("success")
            self.login_badge.add_css_class("dim-label")
            self.login_button.set_label(_("Log in again"))
        else:
            self.login_badge.set_text(_("Not logged in"))
            self.login_badge.remove_css_class("success")
            self.login_badge.add_css_class("dim-label")
            self.login_button.set_label(_("Scan to log in"))

    def _color_row(self, group: Adw.PreferencesGroup, key: str, title: str) -> None:
        row = Adw.ActionRow(title=title)
        button = Gtk.ColorDialogButton(dialog=Gtk.ColorDialog())
        button.set_valign(Gtk.Align.CENTER)
        button.set_rgba(_to_rgba(str(self.values.get(key) or config.DEFAULTS[key])))
        button.connect("notify::rgba", lambda b, _p: self._set(key, _to_hex(b.get_rgba())))
        row.add_suffix(button)
        group.add(row)
        self._color_buttons[key] = button

    def _apply_preset(self, name_hex: str, text_hex: str, panel_hex: str) -> None:
        self._loading = True
        for key, value in (
            ("name_color", name_hex),
            ("text_color", text_hex),
            ("panel_color", panel_hex),
        ):
            button = self._color_buttons.get(key)
            if button is not None:
                button.set_rgba(_to_rgba(value))
        self._loading = False
        self._set("name_color", name_hex)
        self._set("text_color", text_hex)
        self._set("panel_color", panel_hex)
        self._preview.queue_draw()

    def _on_medal_toggled(self, row: Adw.SwitchRow, _param) -> None:
        if not self._loading:
            self._set("medal", row.get_active())

    def _on_anchor_toggled(self, button: Gtk.ToggleButton, name: str) -> None:
        if button.get_active():
            self._set("anchor", name)

    def _on_layer_selected(self, row: Adw.ComboRow, _param) -> None:
        index = row.get_selected()
        if 0 <= index < len(LAYERS):
            self._set("layer", LAYERS[index][0])

    def _sync_sliders(self) -> None:
        """把 values 里的值刷回滑块上（拖完方块、外部改配置之后用）。"""
        self._loading = True
        for key in ("zoom", "offset_x", "offset_y", "margin"):
            slider = self._sliders.get(key)
            if slider is None:
                continue
            raw = self.values.get(key)
            if key == "zoom":
                value = float(raw or 1.0) * 100
            else:
                value = float(raw or 0)
            # 夹在滑块量程里，免得设成 999 之后滑块跟实际不一样
            adjustment = slider.get_adjustment()
            value = min(max(value, adjustment.get_lower()), adjustment.get_upper())
            slider.set_value(value)
        self._loading = False

    def apply_position(self, anchor: str, offset_x: int, offset_y: int, redraw: bool = True) -> None:
        """九宫格 + 微调一起改（拖方块就是走这条路）。"""
        self._loading = True
        self.values["anchor"] = anchor
        self.values["offset_x"] = offset_x
        self.values["offset_y"] = offset_y
        button = self.anchor_buttons.get(anchor)
        if button is not None:
            button.set_active(True)
        self._loading = False
        self._sync_sliders()
        self._schedule_save()
        self.sketch.queue_draw()

    def _detect_outputs(self) -> dict[str, tuple[int, int]]:
        """先问 GTK 要显示器（KDE/GNOME/niri/sway/X11 都走这条），
        再拿 niri/wlroots/Hyprland 的命令补一补——GTK 认不出的名字只有它们知道。"""
        found: dict[str, tuple[int, int]] = {}
        try:
            display = self.get_display() or Gdk.Display.get_default()
            monitors = display.get_monitors() if display is not None else None
            for index in range(monitors.get_n_items() if monitors is not None else 0):
                monitor = monitors.get_item(index)
                name = None
                for getter in ("get_connector", "get_model"):
                    try:
                        name = getattr(monitor, getter)()
                    except (AttributeError, TypeError):  # 老 GTK 没有 get_connector
                        name = None
                    if name:
                        break
                geometry = monitor.get_geometry()
                if name and geometry.width > 0:
                    found.setdefault(name, (geometry.width, geometry.height))
        except Exception:  # pragma: no cover - 没有显示后端就算了
            pass
        for name, size in service.output_sizes().items():
            found.setdefault(name, size)
        return found

    def _screen_size(self) -> tuple[int, int]:
        """目标显示器（没指定就第一块）的逻辑分辨率，问不到就 16:9。"""
        target = self.values.get("output")
        if target and target in self.outputs:
            return self.outputs[target]
        if self.outputs:
            return next(iter(self.outputs.values()))
        return (1920, 1080)

    def _send_test_danmaku(self) -> None:
        """往浮层盯着的文件里写几行样例，屏幕上立刻能看到效果。"""
        try:
            config.send_test_danmaku()
        except OSError as error:
            self.toast(_("Command failed: {err}").format(err=error))
            return
        self.toast(_("Test danmaku sent"))

    def _on_output_selected(self, row: Adw.ComboRow, _param) -> None:
        if self._loading:
            return
        index = row.get_selected()
        model = row.get_model()
        name = None if index == 0 else model.get_string(index)
        self._set("output", name)

    def _on_autostart_toggled(self, row: Adw.SwitchRow, _param) -> None:
        if self._loading:
            return
        ok, output = service.set_enabled(row.get_active())
        if not ok:
            self.toast(_("Command failed: {err}").format(err=output))
        self.refresh_status()

    # ---------- 服务 ----------

    def _service_action(self, action: str) -> None:
        ok, output = getattr(service, action)()
        if not ok:
            self.toast(_("Command failed: {err}").format(err=output))
        elif action == "restart":
            self.toast(_("Overlay restarted"))
        self.refresh_status()

    def _poll_status(self) -> bool:
        self.refresh_status()
        return True

    def refresh_status(self) -> None:
        """由 app 和定时器调：更新运行状态那几行。"""
        if not hasattr(self, "status_label"):
            return
        installed = service.unit_exists()
        active = installed and service.is_active()
        text = _("Running") if active else (_("Stopped") if installed else _("Not installed"))
        self._loading = True
        self.autostart_row.set_active(installed and service.is_enabled())
        self._loading = False
        self.status_label.set_text(text)
        # 运行中要一眼看得出来：绿点 + 粗体；没跑就淡一点，未安装标红
        for widget in (self.status_label, self.status_icon):
            for cls in ("heading", "success", "warning", "error"):
                widget.remove_css_class(cls)
        if active:
            for cls in ("caption", "dim-label"):
                self.status_label.remove_css_class(cls)
            self.status_label.add_css_class("heading")
            self.status_icon.add_css_class("success")
        else:
            self.status_label.add_css_class("caption")
            self.status_label.add_css_class("dim-label")
            self.status_icon.add_css_class("warning" if installed else "error")
        self._sync_login()   # 重启界面后也要按 cookie 显示「已登录」
        for button in (self.start_button, self.stop_button, self.restart_button):
            button.set_sensitive(installed)
        self.autostart_row.set_sensitive(installed)

    # ---------- 杂项 ----------

    def toast(self, text: str) -> None:
        self._toast.add_toast(Adw.Toast(title=text, timeout=3))

