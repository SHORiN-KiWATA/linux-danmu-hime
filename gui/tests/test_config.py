"""config.py 的单元测试：跑 `python3 -m unittest discover -s tests`（或者 pytest tests）。"""

from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))


class ConfigTest(unittest.TestCase):
    def setUp(self) -> None:
        self.home = tempfile.TemporaryDirectory()
        self._old = os.environ.get("XDG_CONFIG_HOME")
        os.environ["XDG_CONFIG_HOME"] = self.home.name
        # 换过环境变量之后要重新拿路径
        import importlib

        from danmu_hime import config

        self.config = importlib.reload(config)

    def tearDown(self) -> None:
        if self._old is None:
            os.environ.pop("XDG_CONFIG_HOME", None)
        else:
            os.environ["XDG_CONFIG_HOME"] = self._old
        self.home.cleanup()

    def test_path_follows_xdg(self) -> None:
        self.assertEqual(
            self.config.config_path(),
            Path(self.home.name) / "danmu-hime" / "config.json",
        )

    def test_missing_file_gives_defaults(self) -> None:
        values = self.config.load()
        self.assertEqual(values["anchor"], "bottom-right")
        self.assertEqual(values["font_size"], 20.0)
        self.assertIsNone(values["emoji_font"])

    def test_save_then_load_round_trip(self) -> None:
        values = self.config.load()
        values.update({"room": "14709735", "font_size": 26.0, "opacity": 0.4})
        self.config.save(values)
        again = self.config.load()
        self.assertEqual(again["room"], "14709735")
        self.assertEqual(again["font_size"], 26.0)
        self.assertEqual(again["opacity"], 0.4)

    def test_save_keeps_every_key(self) -> None:
        self.config.save({"room": "1"})
        raw = json.loads(self.config.config_path().read_text(encoding="utf-8"))
        self.assertEqual(sorted(raw), sorted(self.config.DEFAULTS))

    def test_broken_file_falls_back(self) -> None:
        path = self.config.config_path()
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("{ 这不是 json", encoding="utf-8")
        self.assertEqual(self.config.load()["width"], 420)

    def test_restart_only_changes(self) -> None:
        old = {"room": "1", "font": None, "font_size": 20.0}
        new = {"room": "2", "font": None, "font_size": 24.0}
        self.assertEqual(self.config.restart_only_changed(old, new), ["room"])

    def test_live_change_is_not_restart_only(self) -> None:
        old = self.config.load()
        new = dict(old, opacity=0.3, ttl=20.0)
        self.assertEqual(self.config.restart_only_changed(old, new), [])


class LoginTest(unittest.TestCase):
    def test_import_reads_hime_config(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "app-config.json"
            path.write_text(
                json.dumps({"cookies": [{"name": "SESSDATA", "value": "x%2Cy"},
                                        {"name": "DedeUserID", "value": "9202840"}]}),
                encoding="utf-8",
            )
            from danmu_hime import config as module

            cookie = module.import_from_hime(path)
            self.assertEqual(cookie, "SESSDATA=x%2Cy; DedeUserID=9202840")

    def test_import_without_sessdata_is_none(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "app-config.json"
            path.write_text(json.dumps({"cookies": [{"name": "buvid3", "value": "1"}]}))
            from danmu_hime import config as module

            self.assertIsNone(module.import_from_hime(path))

    def test_login_state(self) -> None:
        from danmu_hime import config as module

        self.assertEqual(module.login_state({"cookie": None}), ("anonymous", 0))
        self.assertEqual(
            module.login_state({"cookie": "SESSDATA=a; DedeUserID=9202840"}),
            ("logged-in", 9202840),
        )


class I18nTest(unittest.TestCase):
    def test_every_key_has_a_translation(self) -> None:
        from danmu_hime import i18n

        missing = [key for key, value in i18n._ZH.items() if not value]
        self.assertEqual(missing, [])

    def test_unknown_string_passes_through(self) -> None:
        from danmu_hime import i18n

        self.assertEqual(i18n._("没有这个词的字符串"), "没有这个词的字符串")


class ServiceTest(unittest.TestCase):
    def test_module_does_not_shell_out_at_import(self) -> None:
        from danmu_hime import service

        self.assertTrue(service.UNIT.endswith(".service"))
        self.assertIn("journalctl", service.LOG_COMMAND)


if __name__ == "__main__":
    unittest.main()
