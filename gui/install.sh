#!/usr/bin/env bash
# 把 GUI 装到 ~/.local，并生成浮层的 systemd 用户单元。
#
#   ./install.sh                     # 浮层二进制自动找（仓库 target/release 或 PATH）
#   ./install.sh --overlay /path/to/danmu-hime
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
BINDIR="$PREFIX/bin"
APPDIR="$PREFIX/share/danmu-hime-config"
APPS="$PREFIX/share/applications"
ICONS="$PREFIX/share/icons/hicolor/128x128/apps"
UNITDIR="$HOME/.config/systemd/user"
HERE="$(cd "$(dirname "$0")" && pwd)"

overlay=""
while [ $# -gt 0 ]; do
    case "$1" in
    --overlay)
        overlay="${2:?--overlay 后面要给路径}"
        shift 2
        ;;
    -h | --help)
        sed -n '2,6p' "$0"
        exit 0
        ;;
    *)
        echo "不认识的参数：$1" >&2
        exit 1
        ;;
    esac
done

if [ -z "$overlay" ]; then
    for candidate in \
        "$HERE/../target/release/danmu-hime" \
        "$HERE/target/release/danmu-hime" \
        "$(command -v danmu-hime || true)"; do
        if [ -n "$candidate" ] && [ -x "$candidate" ]; then
            overlay="$candidate"
            break
        fi
    done
fi
if [ -z "$overlay" ]; then
    echo "找不到 danmu-hime，先 cargo build --release，或用 --overlay 指定" >&2
    exit 1
fi
overlay="$(readlink -f "$overlay")"

echo "浮层二进制：$overlay"
mkdir -p "$BINDIR" "$APPDIR" "$APPS" "$ICONS" "$UNITDIR"
# 符号链接而不是拷贝：以后重新 cargo build 就是新的
ln -sf "$overlay" "$BINDIR/danmu-hime"

rm -rf "$APPDIR/danmu_hime"
cp -r "$HERE/danmu_hime" "$APPDIR/"
cat >"$BINDIR/danmu-hime-config" <<LAUNCH
#!/usr/bin/env python3
import sys

sys.path.insert(0, "$APPDIR")

from danmu_hime.app import main

if __name__ == "__main__":
    sys.exit(main())
LAUNCH
chmod 0755 "$BINDIR/danmu-hime-config"
install -m 0644 "$HERE/io.github.shorin_kiwata.DanmuHime.desktop" "$APPS/"
install -m 0644 "$HERE/io.github.shorin_kiwata.DanmuHime.desktop" "$APPS/"
install -m 0644 "$HERE/icons/io.github.shorin_kiwata.DanmuHime.png" "$ICONS/"

cat >"$UNITDIR/danmu-hime.service" <<UNIT
[Unit]
Description=bilibili 直播弹幕浮层
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart=$BINDIR/danmu-hime
Restart=on-failure
RestartSec=3
Slice=session.slice

[Install]
WantedBy=default.target
UNIT

if command -v systemctl >/dev/null; then
    systemctl --user daemon-reload || true
fi
update-desktop-database "$APPS" >/dev/null 2>&1 || true

cat <<EOF
装好了：
  设置界面    $BINDIR/danmu-hime-config      （应用菜单里叫「弹幕浮层设置」）
  浮层        $BINDIR/danmu-hime     → $overlay
  配置        \${XDG_CONFIG_HOME:-$HOME/.config}/danmu-hime/config.json
  服务单元    $UNITDIR/danmu-hime.service

开机自启：设置界面第一页的「开机自启」开关，或者
  systemctl --user enable --now danmu-hime.service
EOF
