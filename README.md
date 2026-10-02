# linux-danmu-hime

把 B 站直播弹幕画在屏幕上的 Wayland 浮层（wlr-layer-shell），自己画（软件渲染，
无 GPU 依赖），点击穿透；配套一个 GTK4 设置界面。

<p align="center">
  <img src="docs/logo.png" width="150" alt="logo">
</p>

<p align="center">
  <img src="docs/demo.png" width="420" alt="演示：浮层里的弹幕（含 test 弹幕、换行、emoji）">
</p>

## 装

```bash
# AUR（Arch）
yay -S linux-danmu-hime

# 或者从源码
cargo build --release --bin danmu-hime
cd gui && ./install.sh --overlay ../target/release/danmu-hime
```

## 用

```bash
danmu-hime 721              # 浮层：房间号（或直播间链接）
danmu-hime-config           # 设置界面（应用菜单里叫「弹幕浮层设置」）
```

界面里的改动写进 `~/.config/bilibili-danmaku/config.json` 后**立即生效**（浮层每帧看
一眼 mtime），只有房间号 / cookie / 字体 / 显示器要重启浮层，改完会自己重启。

- 昵称打码（`安***`）是服务端行为，**扫码登录**之后就正常了
- 粉丝牌子 `[牌子 · 等级]`、礼物、醒目留言都能画，也能各自关掉
- 字号、位置、行距、停留、淡出、配色全在设置界面里，位置直接拖方块

## 依赖

`gtk4` `libadwaita` `python-gobject`（界面）；`qrencode`（扫码登录画二维码，可选）；
彩色 emoji 需要 `noto-fonts-emoji`（可选）。

MIT
