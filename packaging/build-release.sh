#!/usr/bin/env bash
# 打「预编译包」的 tarball 并发 GitHub release。
# 用法：packaging/build-release.sh 0.1.4 0.1.3
#       参数1=新版本号  参数2=上一个版本号（拿它的文件树当底子）
set -euo pipefail
new=${1:?新版本号}; old=${2:?上一个版本号}
cd "$(dirname "$0")/.."
sed -i "s/^version = \"$old\"/version = \"$new\"/" Cargo.toml
cargo build --release --offline --bin danmu-hime
work=$(mktemp -d)
tar xzf "packaging/danmu-hime-$old-x86_64.tar.gz" -C "$work"
mv "$work/danmu-hime-$old" "$work/danmu-hime-$new"
stage="$work/danmu-hime-$new"
install -Dm0755 target/release/danmu-hime "$stage/usr/bin/danmu-hime"
strip --strip-all "$stage/usr/bin/danmu-hime"
moddir=$(dirname "$(find "$stage" -name window.py | head -1)")
cp -a gui/danmu_hime/*.py "$moddir/"
find "$stage" -name __pycache__ -prune -exec rm -rf {} + 2>/dev/null || true
tar czf "packaging/danmu-hime-$new-x86_64.tar.gz" -C "$work" "danmu-hime-$new"
rm -rf "$work"
echo "sha256=$(sha256sum "packaging/danmu-hime-$new-x86_64.tar.gz" | cut -d' ' -f1)"
echo "接着：gh release create v$new packaging/danmu-hime-$new-x86_64.tar.gz --title v$new"
echo "然后改 packaging/PKGBUILD 的 pkgver/sha256sums，makepkg --printsrcinfo > .SRCINFO，推 ~/Documents/aur/linux-danmu-hime"
