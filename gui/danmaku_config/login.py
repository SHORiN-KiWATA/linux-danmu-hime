"""B 站扫码登录：拿二维码 → 轮询 → 换出 cookie。

用的是网页版登录那套接口（passport.bilibili.com），登录成功后 cookie 落在响应头上，
抓下来写进 config.json 就行。所有网络调用都是阻塞的，调用方自己开线程。
"""

from __future__ import annotations

import http.cookiejar
import json
import shutil
import subprocess
import urllib.parse
import urllib.request

GENERATE = "https://passport.bilibili.com/x/passport-login/web/qrcode/generate"
POLL = "https://passport.bilibili.com/x/passport-login/web/qrcode/poll?qrcode_key="
HEADERS = {
    "User-Agent": (
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) "
        "Chrome/126.0.0.0 Safari/537.36"
    ),
    "Referer": "https://www.bilibili.com/",
}

# 轮询返回的 data.code → 我们这边的状态
STATES = {0: "confirmed", 86038: "expired", 86090: "scanned", 86101: "waiting"}

# 认证要用的那几个 cookie（跟直播姬存的是同一批）
COOKIE_NAMES = ("SESSDATA", "bili_jct", "DedeUserID", "DedeUserID__ckMd5", "sid")


def generate() -> tuple[str, str]:
    """申请一次扫码会话，返回 (二维码内容, 轮询用的 key)。"""
    request = urllib.request.Request(GENERATE, headers=HEADERS)
    with urllib.request.urlopen(request, timeout=10) as response:
        payload = json.load(response)
    data = payload.get("data") or {}
    url, key = data.get("url"), data.get("qrcode_key")
    if not url or not key:
        raise OSError(f"生成二维码失败：{payload.get('message') or payload}")
    return url, key


def poll(key: str) -> tuple[str, str | None]:
    """轮询一次：返回 (状态, cookie)。状态：waiting / scanned / confirmed / expired。"""
    jar = http.cookiejar.CookieJar()
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
    request = urllib.request.Request(POLL + urllib.parse.quote(key), headers=HEADERS)
    with opener.open(request, timeout=10) as response:
        payload = json.load(response)
        # cookie jar 有时接不住（同一响应里既有跳转又有 Set-Cookie），
        # 这里自己再抠一遍响应头，别白扫一次码。
        raw_headers = response.headers.get_all("Set-Cookie") or []
    data = payload.get("data") or {}
    state = STATES.get(data.get("code"), "waiting")
    if state != "confirmed":
        return state, None

    # 有些账号的 cookie 是落在 www.bilibili.com 上的，再走一趟把它捎回来
    if not any(cookie.name == "SESSDATA" for cookie in jar):
        try:
            opener.open(urllib.request.Request("https://www.bilibili.com/", headers=HEADERS)).close()
        except OSError:
            pass

    values = {cookie.name: cookie.value for cookie in jar}
    for header in raw_headers:
        piece = header.split(";", 1)[0].strip()
        name, _, value = piece.partition("=")
        if name and value:
            values.setdefault(name, value)
    pairs = [f"{name}={values[name]}" for name in COOKIE_NAMES if values.get(name)]
    if not any(pair.startswith("SESSDATA=") for pair in pairs):
        return "confirmed", None
    return "confirmed", "; ".join(pairs)


def verify(cookie: str) -> tuple[int, str]:
    """拿 cookie 问一下 nav：返回 (uid, 昵称)。没登录就 (0, "")。"""
    request = urllib.request.Request(
        "https://api.bilibili.com/x/web-interface/nav",
        headers={**HEADERS, "Cookie": cookie},
    )
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            payload = json.load(response)
    except OSError:
        return (0, "")
    data = payload.get("data") or {}
    return (int(data.get("mid") or 0), str(data.get("uname") or ""))


def qr_png(text: str, scale: int = 8) -> bytes | None:
    """用 qrencode 画一张 PNG 出来；机器上没装就返回 None（界面退回显示链接）。"""
    if shutil.which("qrencode") is None:
        return None
    try:
        proc = subprocess.run(
            ["qrencode", "-t", "PNG", "-s", str(scale), "-m", "2", "-o", "-", text],
            capture_output=True,
            timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired):  # pragma: no cover - 环境问题
        return None
    return proc.stdout or None
