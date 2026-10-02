"""通过 systemd --user 控制浮层：启动/停止/重启/开机自启。

单元文件由 gui/install.sh 生成（~/.config/systemd/user/danmu-hime.service）。
"""

from __future__ import annotations

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
