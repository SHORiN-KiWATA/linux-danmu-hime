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
SAMPLES = [
    ("晴晴了", "近亲走过去吃大"),
    ("[瑞乃酱·6]", "这条全是 emoji：😀😂🔥✨🎉💯"),
    ("毒树之果", "下路让你轮上了，中文随便断但英文 words must not be split"),
]

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
    """1:1 画一块浮层底部出来：字号、行距、底板透明度、换行都是真的。"""

    def __init__(self, window: "ConfigWindow") -> None:
        super().__init__()
        self.window = window
        self.set_size_request(-1, 300)
        self.set_draw_func(self._draw)

    def _draw(self, _area, cr, width: int, height: int) -> None:
        values = self.window.values
        font_size = max(6.0, float(values["font_size"]))
        gap = float(values["line_gap"])
        opacity = max(0.0, min(1.0, float(values["opacity"])))
        pad = 6.0

        cr.set_source_rgb(0.10, 0.11, 0.13)
        cr.paint()

        panel_width = min(float(width), float(values["width"]))
        desc = _pango_font(values.get("font"), font_size)
        # 先量一遍：一条弹幕软换行成几行，就往排几行
        rows = []
        cursor = pad
        for name, text in SAMPLES:
            prefix = self.create_pango_layout(f"{name}:")
            prefix.set_font_description(desc)
            prefix_width = prefix.get_pixel_size()[0]
            body = self.create_pango_layout(text)
            body.set_font_description(desc)
            body.set_width(int(panel_width - pad * 2 - prefix_width) * Pango.SCALE)
            body.set_wrap(Pango.WrapMode.CHAR)
            body.set_spacing(int(gap * Pango.SCALE))
            row_height = max(prefix.get_pixel_size()[1], body.get_pixel_size()[1])
            rows.append((cursor, prefix, prefix_width, body))
            cursor += row_height + gap
        panel_height = cursor - gap + pad
        left = width - panel_width if "right" in values["anchor"] else 0.0
        top = height - panel_height

        # 底板
        panel = _rgb(values.get("panel_color") or config.DEFAULTS["panel_color"])
        cr.set_source_rgba(panel[0], panel[1], panel[2], 0.55 * opacity)
        cr.rectangle(left, top, panel_width, panel_height)
        cr.fill()

        # 字：昵称淡蓝、内容近白（跟 render.rs 一致）
        name = _rgb(values.get("name_color") or config.DEFAULTS["name_color"])
        text_rgb = _rgb(values.get("text_color") or config.DEFAULTS["text_color"])
        for row_top, prefix, prefix_width, body in rows:
            line_top = top + row_top
            cr.set_source_rgba(name[0], name[1], name[2], 1.0)
            cr.move_to(left + pad, line_top)
            PangoCairo.show_layout(cr, prefix)
            cr.set_source_rgba(text_rgb[0], text_rgb[1], text_rgb[2], 1.0)
            cr.move_to(left + pad + prefix_width, line_top)
            PangoCairo.show_layout(cr, body)


class ScreenSketch(Gtk.DrawingArea):
    """一小块屏幕示意：浮层挂在哪、离边多远、占多大，一眼能看出来。"""

    def __init__(self, window: "ConfigWindow") -> None:
        super().__init__()
        self.window = window
        self.set_size_request(300, 160)
        self.set_draw_func(self._draw)

    def _draw(self, _area, cr, width: int, height: int) -> None:
        values = self.window.values
        pad = 6.0
        cr.set_source_rgb(0.16, 0.17, 0.20)
        cr.paint()
        cr.set_source_rgb(0.10, 0.11, 0.13)
        cr.rectangle(pad, pad, width - pad * 2, height - pad * 2)
        cr.fill()

        inner_w = width - pad * 2
        inner_h = height - pad * 2
        scale = min(
            1.0,
            inner_w / max(1.0, float(values["width"])),
            inner_h / max(1.0, float(values["height"])),
        )
        box_w = max(6.0, float(values["width"]) * scale)
        box_h = max(6.0, float(values["height"]) * scale)
        margin = min(float(values["margin"]) * scale, min(inner_w, inner_h) / 2 - 3)
        anchor = values["anchor"]
        x = pad + margin if "left" in anchor else (
            width - pad - margin - box_w if "right" in anchor else (width - box_w) / 2
        )
        y = pad + margin if "top" in anchor else (
            height - pad - margin - box_h if "bottom" in anchor else (height - box_h) / 2
        )
        x += float(values.get("offset_x") or 0) * scale
        y += float(values.get("offset_y") or 0) * scale
        cr.set_source_rgba(0.35, 0.62, 1.0, 0.45)
        cr.rectangle(x, y, box_w, box_h)
        cr.fill()
        cr.set_source_rgba(0.55, 0.75, 1.0, 0.85)
        cr.set_line_width(1.0)
        cr.rectangle(x + 0.5, y + 0.5, max(1.0, box_w - 1), max(1.0, box_h - 1))
        cr.stroke()


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
        self._loading = True
        self._save_timer = 0
        self._sliders: dict[str, Gtk.Scale] = {}
        self._toast = Adw.ToastOverlay()
        self._build_ui()
        self._loading = False
        self._sync_scale_slider()
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
        status_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        status_box.append(Gtk.Image.new_from_icon_name("media-record-symbolic"))
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
        self.cookie_row = Adw.EntryRow(title=_("Cookie (optional)"))
        self.cookie_row.set_text(str(self.values.get("cookie") or ""))
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
        for button in (self.start_button, self.stop_button, self.restart_button):
            buttons.append(button)

        self.process_row = Adw.ActionRow(title=_("Overlay process"))
        self.process_row.add_suffix(buttons)
        room_group.add(self.process_row)

        self.autostart_row = Adw.SwitchRow(title=_("Start at login"))
        self.autostart_row.connect("notify::active", self._on_autostart_toggled)
        room_group.add(self.autostart_row)
        page.add(room_group)

        anchor_group = Adw.PreferencesGroup(title=_("Position settings"))
        grid = Gtk.Grid()
        grid.set_row_spacing(4)
        grid.set_column_spacing(4)
        grid.set_halign(Gtk.Align.CENTER)
        grid.set_margin_top(6)
        grid.set_margin_bottom(6)
        self.anchor_buttons: dict[str, Gtk.ToggleButton] = {}
        first: Gtk.ToggleButton | None = None
        for index, (name, glyph) in enumerate(ANCHORS):
            button = Gtk.ToggleButton(label=glyph)
            button.set_tooltip_text(_(f"anchor-{name}"))
            button.add_css_class("flat")
            button.set_size_request(46, -1)
            if first is None:
                first = button
            else:
                button.set_group(first)
            button.set_active(self.values["anchor"] == name)
            button.connect("toggled", self._on_anchor_toggled, name)
            grid.attach(button, index % 3, index // 3, 1, 1)
            self.anchor_buttons[name] = button
        anchor_group.add(WidgetRow(grid, margin=8))

        self.sketch = ScreenSketch(self)
        self.sketch.set_halign(Gtk.Align.CENTER)
        anchor_group.add(WidgetRow(self.sketch, margin=10))

        self._add_slider(
            anchor_group, "margin", _("Margin"), 0, 240, 2, "px", subtitle=_("Margin hint")
        )
        self._add_slider(
            anchor_group, "offset_x", _("Offset X"), -400, 400, 2, "px", subtitle=_("Offset X hint")
        )
        self._add_slider(
            anchor_group, "offset_y", _("Offset Y"), -400, 400, 2, "px", subtitle=_("Offset Y hint")
        )

        layer_row = Adw.ComboRow(title=_("Layer"))
        layer_row.set_subtitle(_("Layer hint"))
        layer_row.set_model(Gtk.StringList.new([_(key) for _name, key in LAYERS]))
        layer_row.set_selected([name for name, _key in LAYERS].index(self.values["layer"]))
        layer_row.connect("notify::selected", self._on_layer_selected)
        anchor_group.add(layer_row)
        self.output_row = self._entry_row(anchor_group, "output", _("Output"), None)
        self.scale_row = Adw.SwitchRow(title=_("Manual scale"), subtitle=_("Scale hint"))
        self.scale_row.set_active(self.values.get("scale") is not None)
        self.scale_row.connect("notify::active", self._on_scale_toggled)
        anchor_group.add(self.scale_row)
        self._add_slider(anchor_group, "scale", _("Scale"), 50, 300, 5, "%")
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
        self.medal_row = Adw.SwitchRow(title=_("Show medal"), subtitle=_("Show medal hint"))
        self.medal_row.set_active(bool(self.values.get("medal", True)))
        self.medal_row.connect("notify::active", self._on_medal_toggled)
        font_group.add(self.medal_row)
        self._add_slider(font_group, "font_size", _("Font size"), 10, 48, 1, "px")
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
            self.login_badge.set_text(_("Logged in as {uid}").format(uid=uid or "?"))
            self.login_button.set_label(_("Log in again"))
        else:
            self.login_badge.set_text(_("Not logged in"))
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

    def _on_scale_toggled(self, row: Adw.SwitchRow, _param) -> None:
        if self._loading:
            return
        slider = self._sliders.get("scale")
        if slider is not None:
            slider.set_sensitive(row.get_active())
        self._set("scale", (slider.get_value() / 100.0) if row.get_active() and slider else None)

    def _sync_scale_slider(self) -> None:
        slider = self._sliders.get("scale")
        if slider is not None:
            slider.set_sensitive(self.scale_row.get_active())

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
        for button in (self.start_button, self.stop_button, self.restart_button):
            button.set_sensitive(installed)
        self.autostart_row.set_sensitive(installed)

    # ---------- 杂项 ----------

    def toast(self, text: str) -> None:
        self._toast.add_toast(Adw.Toast(title=text, timeout=3))

