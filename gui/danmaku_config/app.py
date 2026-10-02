from __future__ import annotations

import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gio  # noqa: E402

from .window import ConfigWindow

APP_ID = "io.github.shorin_kiwata.DanmakuOverlayConfig"


class App(Adw.Application):
    def __init__(self) -> None:
        super().__init__(application_id=APP_ID, flags=Gio.ApplicationFlags.DEFAULT_FLAGS)

    def do_activate(self) -> None:
        win = self.get_active_window()
        if win is None:
            win = ConfigWindow(self)
        win.present()
        win.refresh_status()


def main() -> int:
    return App().run(sys.argv)
