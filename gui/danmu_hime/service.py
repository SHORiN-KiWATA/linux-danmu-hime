"""通过 systemd --user 控制浮层：启动/停止/重启/开机自启。

单元文件由 gui/install.sh 生成（~/.config/systemd/user/danmu-hime.service）。
"""

from __future__ import annotations

import json
import shutil
import subprocess

UNIT = "danmu-hime.service"
LOG_COMMAND = f"journalctl --user -u {UNIT} -f"


def _systemctl(*args: str) -> tuple[bool, str]:
    if shutil.which("systemctl") is None:
        return False, "找不到 systemctl"
    try:
        proc = subprocess.run(
            ["systemctl", "--user", *args], capture_output=True, text=True, timeout=10
        )
    except (OSError, subprocess.TimeoutExpired) as err:  # pragma: no cover - 环境问题
        return False, str(err)
    output = (proc.stdout + proc.stderr).strip()
    return proc.returncode == 0, output


def unit_exists() -> bool:
    ok, _ = _systemctl("cat", UNIT)
    return ok


def _find_size(node, depth: int = 0) -> tuple[int, int] | None:
    """往合成器给的 JSON 里扒第一对像分辨率的 width/height。"""
    if depth > 4:
        return None
    if isinstance(node, dict):
        width, height = node.get("width"), node.get("height")
        if all(isinstance(v, int) and v > 200 for v in (width, height)):
            return (int(width), int(height))
        for value in node.values():
            size = _find_size(value, depth + 1)
            if size:
                return size
    elif isinstance(node, list):
        for item in node:
            size = _find_size(item, depth + 1)
            if size:
                return size
    return None


def output_sizes() -> dict[str, tuple[int, int]]:
    """每块屏的逻辑分辨率（问不到就空着，界面回退 16:9）。"""
    for command in (
        ["niri", "msg", "--json", "outputs"],
        ["wlr-randr", "--json"],
        ["hyprctl", "-j", "monitors"],
    ):
        if shutil.which(command[0]) is None:
            continue
        try:
            proc = subprocess.run(command, capture_output=True, text=True, timeout=4)
            payload = json.loads(proc.stdout or "null")
        except (OSError, subprocess.TimeoutExpired, json.JSONDecodeError):
            continue
        sizes: dict[str, tuple[int, int]] = {}
        if isinstance(payload, dict):
            for name, value in payload.items():
                size = _find_size(value)
                if size:
                    sizes[name] = size
        elif isinstance(payload, list):
            for item in payload:
                if isinstance(item, dict) and item.get("name"):
                    size = _find_size(item)
                    if size:
                        sizes[str(item["name"])] = size
        if sizes:
            return sizes
    return {}


def list_outputs() -> list[str]:
    """问合成器现在有哪几块屏；niri / wlroots / Hyprland 的写法都试一遍。"""
    for command in (
        ["niri", "msg", "--json", "outputs"],
        ["wlr-randr", "--json"],
        ["hyprctl", "-j", "monitors"],
    ):
        if shutil.which(command[0]) is None:
            continue
        try:
            proc = subprocess.run(command, capture_output=True, text=True, timeout=4)
            payload = json.loads(proc.stdout or "null")
        except (OSError, subprocess.TimeoutExpired, json.JSONDecodeError):
            continue
        names: list[str] = []
        if isinstance(payload, dict):
            names = [name for name, value in payload.items() if isinstance(value, (dict, list))]
        elif isinstance(payload, list):
            names = [item.get("name") for item in payload if isinstance(item, dict)]
        names = [name for name in names if name]
        if names:
            return names
    return []


def is_active() -> bool:
    ok, _ = _systemctl("is-active", "--quiet", UNIT)
    return ok


def is_enabled() -> bool:
    ok, _ = _systemctl("is-enabled", "--quiet", UNIT)
    return ok


def start() -> tuple[bool, str]:
    return _systemctl("start", UNIT)


def stop() -> tuple[bool, str]:
    return _systemctl("stop", UNIT)


def restart() -> tuple[bool, str]:
    return _systemctl("restart", UNIT)


def set_enabled(enabled: bool) -> tuple[bool, str]:
    return _systemctl("enable" if enabled else "disable", "--now", UNIT)
