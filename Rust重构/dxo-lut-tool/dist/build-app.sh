#!/bin/bash
# ============================================================================
# DXO-ONE-XLAB · Tauri .app 打包脚本（R6-02）
# ============================================================================
# 构建 release 版 dxo-app 并组装可独立启动的 macOS .app。
# 前端（ui/）与原厂基线表均经 Tauri/include_bytes! 嵌入二进制，无项目目录依赖。
# 数据目录默认 ~/Library/Application Support/DXO-ONE-XLAB，可用 DXO_DATA_DIR 覆盖。
#
# 用法: ./dist/build-app.sh
# 产物: dxo-app/AppBundle/DXO-ONE-XLAB.app
#
# 签名渠道：本地 ad-hoc 签名（--sign -），未做 Developer ID 签名/公证。
# ============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_DIR"

APP="dxo-app/AppBundle/DXO-ONE-XLAB.app"
VERSION="$(grep -m1 '^version' dxo-app/Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
IDENTIFIER="cn.dxoxlab.luttool"

# 发布红线：二进制不得带个人绝对路径（panic 定位串会嵌入 $HOME）。
export RUSTFLAGS="--remap-path-prefix=$HOME=/user${RUSTFLAGS:+ $RUSTFLAGS}"

echo "[1/5] 构建 release dxo-app..."
cargo build --locked --release -p dxo-app

echo "[2/5] 组装 .app 结构..."
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp -f target/release/dxo-app "$APP/Contents/MacOS/dxo-app"
cp -f dxo-app/icons/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
printf 'APPL????' > "$APP/Contents/PkgInfo"

echo "[3/5] 写入 Info.plist（版本 ${VERSION}）..."
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>$IDENTIFIER</string>
  <key>CFBundleName</key><string>DXO-ONE-XLAB</string>
  <key>CFBundleDisplayName</key><string>DXO-ONE-XLAB</string>
  <key>CFBundleExecutable</key><string>dxo-app</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSMinimumSystemVersion</key><string>10.15</string>
</dict>
</plist>
PLIST

echo "[4/5] ad-hoc 签名（本地渠道，未公证）..."
# tauri generate_context! 经 env! 嵌入绝对项目路径，remap-path-prefix 不覆盖 env!；
# 等长字节 scrub 去除个人 home（不改变偏移/长度），随后再签名密封。
python3 - "$APP/Contents/MacOS/dxo-app" <<'PY'
import os, sys
p = sys.argv[1]
home = os.environ["HOME"].encode()
neutral = (b"/Users/USER" + b"_" * len(home))[: len(home)]
d = open(p, "rb").read()
open(p, "wb").write(d.replace(home, neutral))
PY
codesign --force --deep --sign - "$APP"

echo "[5/5] 校验产物..."
test -x "$APP/Contents/MacOS/dxo-app"
test -f "$APP/Contents/Resources/AppIcon.icns"
test -f "$APP/Contents/Info.plist"
if [ "$(strings "$APP/Contents/MacOS/dxo-app" | grep -c "$HOME/" || true)" -gt 0 ]; then
    echo "❌ 二进制仍含个人绝对路径（$HOME）"
    exit 1
fi
echo "  个人路径扫描: 无 ✅"
echo "  结构:"
find "$APP" -type f | sort | sed 's/^/    /'
echo "  二进制: $(stat -f%z "$APP/Contents/MacOS/dxo-app") B"
codesign --verify --deep --strict "$APP"
codesign -dv "$APP" 2>&1 | sed 's/^/  /'

echo ""
echo "完成 ✅  $APP"
echo "干净账户试运行示例:"
echo "  DXO_DATA_DIR=\$(mktemp -d) open \"$APP\""
