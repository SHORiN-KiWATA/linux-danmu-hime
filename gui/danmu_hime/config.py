"""读写 ~/.config/danmu-hime/config.json。

浮层启动时读这个文件，之后每次它被改动都会热重载（显示类设置一秒内生效），
所以 GUI 只要「改哪项就写回文件」就够了，不需要 IPC。
"""

from __future__ import annotations

import json
import os
import tempfile
from pathlib import Path

APP_DIR = "danmu-hime"
CONFIG_NAME = "config.json"

# 跟 Rust 那边的 FileConfig 一一对应；没写的键浮层会用自己的默认值
DEFAULTS: dict[str, object] = {
    "room": "",
    "cookie": None,
    "output": None,
    "anchor": "bottom-right",
    "width": 381,
    "height": 560,
    "margin": 20,
    "offset_x": -20,
    "offset_y": 456,
    "font_size": 28.0,
    "line_gap": 4.0,
    "row_gap": 10.0,
    "opacity": 0.6,
    "ttl": 14.0,
    "fade": 1.0,
    "max_lines": 101,
    "font": None,
    "emoji_font": None,
    "medal": False,
    "gift": True,
    "gift_icon": False,
    "avatar": True,
    "avatar_round": True,
    "name_color": "#ffe58a",
    "text_color": "#ffffff",
    "gift_color": "#ffaad2",
    "panel_color": "#000000",
    "scale": None,
    "zoom": 1.0,
}

# 整数的键（写回文件时别写成 420.0）
INT_KEYS = ("width", "height", "margin", "offset_x", "offset_y", "max_lines")

# 这几项改了必须重启浮层：房间要重连、字体要重新 mmap、显示器要重建 surface
RESTART_KEYS = ("room", "cookie", "output", "font", "emoji_font", "medal", "gift")


def config_path() -> Path:
    base = os.environ.get("XDG_CONFIG_HOME") or str(Path.home() / ".config")
    return Path(base) / APP_DIR / CONFIG_NAME


def load() -> dict:
    """读配置；文件不存在或者坏了都退回默认值，缺的键补默认。"""
    values = dict(DEFAULTS)
    try:
        raw = json.loads(config_path().read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return values
    if isinstance(raw, dict):
        for key in DEFAULTS:
            if key in raw and raw[key] is not None:
                values[key] = coerce(key, raw[key])
    return values


HIME_CONFIG = Path.home() / ".config" / "com.rsplwe.bili-live-hime" / "app-config.json"


def import_from_hime(path: Path | None = None) -> str | None:
    """从「哔哩哔哩直播姬」的配置里抄一份 cookie 出来。

    直播姬是带登录的客户端（认证包里发的是登录 uid），所以它的昵称不会被打码；
    我们借它的 SESSDATA 也是同样的效果。
    """
    path = path or HIME_CONFIG
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    pairs = []
    for item in raw.get("cookies") or []:
        if not isinstance(item, dict):
            continue
        name, value = item.get("name"), item.get("value")
        if name and value:
            pairs.append(f"{name}={value}")
    if not any(pair.startswith("SESSDATA=") for pair in pairs):
        return None
    return "; ".join(pairs)


# 几套现成配色（昵称 / 正文 / 底板）
PRESETS = {
    "tokyonight": ("#7aa2f7", "#c0caf5", "#1a1b26"),
    "catppuccin": ("#89b4fa", "#cdd6f4", "#1e1e2e"),
    "gruvbox": ("#fabd2f", "#ebdbb2", "#282828"),
    "弹幕姬": ("#ffe58a", "#ffffff", "#000000"),
    "默认": ("#84aaff", "#eceef4", "#0a0c12"),
}


# 「测试弹幕」按钮往这个文件里追加行，浮层盯着它读（就在配置文件旁边）
TEST_LINES = [
    "这是一条测试弹幕",
    "[doge] 和 [大笑] 这种表情现在直接画 B 站原图了",
    "短",
    "长一点的弹幕用来看看换行会不会在左边留空格，中文英文混排 mixed with English words 也要能断开",
    "🐶🎉 emoji 和 [大哭] 一起上",
    "测试弹幕也按你设的停留时间自己淡出",
]


def test_path() -> Path:
    return config_path().with_name("test-danmaku.jsonl")


def send_test_danmaku(lines=None) -> Path:
    """追加几行样例弹幕；浮层每秒看一眼这个文件，看到就当成真弹幕画出来。"""
    path = test_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    try:
        if path.stat().st_size > 64 * 1024:
            path.write_text("", encoding="utf-8")
    except OSError:
        pass
    with path.open("a", encoding="utf-8") as handle:
        for line in lines or TEST_LINES:
            handle.write(f"{line}\n")
    return path


def login_state(values: dict) -> tuple[str, int]:
    """(状态, uid)：只看本地 cookie，联网校验留给浮层。"""
    raw = values.get("cookie") or ""
    uid = 0
    for pair in raw.split(";"):
        name, _, value = pair.strip().partition("=")
        if name == "DedeUserID":
            uid = int(value) if value.isdigit() else 0
    if "SESSDATA=" in raw:
        return ("logged-in", uid)
    return ("anonymous", 0)


def coerce(key: str, value):
    """整数项一律存成整数：Rust 那边 width/height/margin/max_lines 是整数类型。"""
    if value is None:
        return None
    if key in INT_KEYS:
        return int(round(float(value)))
    if isinstance(DEFAULTS.get(key), float):
        return float(value)
    return value


def save(values: dict) -> Path:
    """原子写：先写同目录的临时文件再 rename，免得浮层读到半截 JSON。"""
    path = config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = {key: coerce(key, values.get(key, DEFAULTS[key])) for key in DEFAULTS}
    text = json.dumps(payload, ensure_ascii=False, indent=2) + "\n"
    with tempfile.NamedTemporaryFile(
        "w", encoding="utf-8", dir=path.parent, prefix=".config-", suffix=".tmp", delete=False
    ) as handle:
        handle.write(text)
        tmp = Path(handle.name)
    tmp.replace(path)
    return path


def restart_only_changed(old: dict, new: dict) -> list[str]:
    """哪些「只能重启生效」的项变了。"""
    return [key for key in RESTART_KEYS if old.get(key) != new.get(key)]
