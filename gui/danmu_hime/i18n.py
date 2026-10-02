"""Tiny built-in translation table (English base, Simplified Chinese)."""

import locale
import os

_ZH = {
    "Danmaku Overlay": "弹幕浮层",
    "Display": "显示",
    "Position": "位置",
    "Danmaku": "弹幕",
    "Preview": "预览",
    "Preview hint": "底板、字号、行距、透明度都跟浮层一致；位置在「位置」页调",
    "Font": "字体",
    "Emoji font file": "emoji 字体文件",
    "Position settings": "位置设置",
    "Login": "登录",
    "Scan to log in": "扫码登录",
    "Log in again": "重新登录",
    "Scan hint": "用哔哩哔哩 App 扫这个码",
    "Loading QR": "正在拿二维码…",
    "Refresh QR": "刷新二维码",
    "Scanned, confirm on your phone": "扫到了，在手机上确认一下",
    "QR expired": "二维码过期了，点下面刷新",
    "Login failed, try again": "没拿到登录状态，再试一次",
    "Logged in, restart the overlay": "登录成功；按 ⟳ 重启浮层生效",
    "Show medal": "显示粉丝牌子",
    "Show gifts": "显示礼物",
    "Show gifts hint": "「[礼物] 某某 投喂 辣条 ×1」这种；醒目留言一直显示",
    "Colors": "配色",
    "Name color": "昵称颜色",
    "Text color": "正文颜色",
    "Panel color": "底板颜色",
    "Logged in as {name}": "已登录：{name}",
    "Show medal hint": "就是每条前面那个 [牌子名 · 等级]",
    "Login hint": "登录后服务端才给真昵称；不登录有些房间会把昵称打成星号（安***）",
    "Import from Hime": "从直播姬导入",
    "Import hint": "直接抄哔哩哔哩直播姬（bili-live-hime）里已经登录好的 cookie",
    "No Hime login found": "没找到直播姬的登录状态",
    "Imported, restart the overlay": "导入好了；按 ⟳ 重启浮层才生效",
    "Logged in as {uid}": "已登录（uid {uid}）",
    "Not logged in": "未登录",
    "layer-short-top": "顶层",
    "layer-short-overlay": "覆盖层",
    "layer-short-bottom": "底层",
    "layer-short-background": "壁纸之上",
    "Layer hint": "顶层不挡全屏窗口（默认）；覆盖层压住所有窗口；底层在普通窗口下面",
    "Overlay process": "浮层进程",
    "Room hint": "房间号或者直播间链接（https://live.bilibili.com/14709735）",
    "Start the overlay": "启动浮层",
    "Stop the overlay": "停止浮层",
    "Restart the overlay": "重启浮层",
    "Line gap hint": "一条弹幕多行之间的额外间距",
    "Panel opacity hint": "0 就是只有字、没有底板",
    "Size": "尺寸",
    "Anchor hint": "浮层贴在屏幕的哪一块（九宫格选一个）",
    "Margin hint": "离屏幕边缘留多少",
    "Offset X": "左右微调",
    "Offset Y": "上下微调",
    "Offset X hint": "正数往右推，负数往左（在九宫格之上再挪这么多）",
    "Offset Y hint": "正数往下推，负数往上",
    "Screen": "屏幕示意",
    "Manual scale": "手动缩放",
    "Scale hint": "关掉就跟随合成器；分数缩放的花屏上可以手动定",
    "Scale": "缩放",
    "Copy": "复制",
    "Copied": "已复制",
    "Command failed: {err}": "命令失败：{err}",
    "anchor-top-left": "左上",
    "anchor-top": "上",
    "anchor-top-right": "右上",
    "anchor-left": "左",
    "anchor-center": "中间",
    "anchor-right": "右",
    "anchor-bottom-left": "左下",
    "anchor-bottom": "下",
    "anchor-bottom-right": "右下",
    "layer-top": "顶层（不挡全屏窗口，默认）",
    "layer-overlay": "覆盖层（压住所有窗口）",
    "layer-bottom": "底层",
    "layer-background": "壁纸之上",
    "Live room": "直播间",
    "Room": "房间号 / 直播间链接",
    "Cookie (optional)": "Cookie（可选）",
    "Cookie hint": "不填就用 bilibili_live_stream 脚本的缓存；登录后弹幕更全",
    "Appearance": "外观",
    "Font size": "字号",
    "Line gap": "行距",
    "Panel opacity": "底板不透明度",
    "Width": "浮层宽",
    "Height": "高度上限",
    "Margin": "距屏幕边缘",
    "Anchor": "贴哪个角",
    "Layer": "层级",
    "Output": "显示器",
    "Output hint": "留空让合成器挑；填名字比如 DP-2（niri msg outputs 能看到）",
    "Timing": "弹幕节奏",
    "Time to live": "弹幕停留",
    "Fade": "淡出时长",
    "Max lines": "最多记多少条",
    "Font file": "字体文件",
    "Choose…": "选择…",
    "Choose a font file": "选择字体文件",
    "Font hint": "留空用 fontconfig 找中文字体",
    "Service": "运行",
    "Status": "状态",
    "Running": "运行中",
    "Stopped": "已停止",
    "Not installed": "没装 systemd 单元（跑一下 gui/install.sh）",
    "Start at login": "开机自启",
    "Start": "启动",
    "Stop": "停止",
    "Restart overlay": "重启浮层",
    "Restart": "重启",
    "Applied": "已应用（浮层热重载）",
    "Applied, restart needed": "已保存；房间/cookie/字体/显示器要重启才生效",
    "Overlay restarted": "浮层已重启",
    "Could not write the config: {err}": "写配置文件失败：{err}",
    "Logs": "看日志",
    "Command": "journalctl --user -u danmu-hime -f",
    "seconds": "秒",
    "pixels": "像素",
    "px": "px",
    "s": "秒",
    "lines": "行",
}
_EN_SPECIAL = {}


def _is_zh() -> bool:
    for var in ("LC_ALL", "LC_MESSAGES", "LANG"):
        v = os.environ.get(var)
        if v:
            return v.lower().startswith("zh")
    try:
        loc = locale.getlocale()[0]
        return bool(loc) and loc.lower().startswith("zh")
    except Exception:
        return False


_ZH_ACTIVE = _is_zh()


def _(s: str) -> str:
    if _ZH_ACTIVE:
        return _ZH.get(s, _EN_SPECIAL.get(s, s))
    return _EN_SPECIAL.get(s, s)
