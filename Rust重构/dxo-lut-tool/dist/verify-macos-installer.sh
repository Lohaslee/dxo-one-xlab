#!/bin/bash
# ============================================================================
# DXO-ONE-XLAB · macOS DMG 安装验收（不触碰真实相机卡，不写用户数据目录）
# ============================================================================
# 覆盖：挂载 → 拖入模拟 Applications → 从安装位置启动 → 干净数据目录 → 覆盖安装保留数据。
# 数据目录一律指向临时目录（DXO_DATA_DIR），绝不使用本机 ~/Library/Application Support。
# 用法: ./dist/verify-macos-installer.sh [dmg 路径]
# 截图: SHOTS_DIR（默认 docs/verify/installer-build-<date>/screenshots）
# ============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
OUT_DIR="$SCRIPT_DIR/release"
SHOTS_DIR="${SHOTS_DIR:-$PROJECT_DIR/docs/verify/installer-build-20261119/screenshots}"
VERSION="$(grep -m1 '^version' "$PROJECT_DIR/dxo-app/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')"
DMG="${1:-$OUT_DIR/DXO-ONE-XLAB-${VERSION}-macOS-$(uname -m).dmg}"
WORK="$(mktemp -d "${TMPDIR:-/tmp/}dxo-inst.XXXXXX")"
APP_NAME="DXO-ONE-XLAB.app"
PID=""

cleanup() {
  [ -n "$PID" ] && kill "$PID" 2>/dev/null || true
  hdiutil detach "$WORK/mnt" -force >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT
mkdir -p "$SHOTS_DIR"

echo "[1/7] 挂载 DMG: $DMG"
mkdir -p "$WORK/mnt"
hdiutil attach -nobrowse -readonly -mountpoint "$WORK/mnt" "$DMG" >/dev/null
[ -d "$WORK/mnt/$APP_NAME" ] || { echo "DMG 内缺少 $APP_NAME"; exit 1; }
ls -la "$WORK/mnt" | sed 's/^/  /'
spctl -a -t exec -vv "$WORK/mnt/$APP_NAME" 2>&1 | sed 's/^/  Gatekeeper: /' || true

echo "[2/7] 模拟「拖入应用程序」安装到 $WORK/Applications"
mkdir -p "$WORK/Applications"
cp -R "$WORK/mnt/$APP_NAME" "$WORK/Applications/$APP_NAME"
codesign --verify --deep --strict "$WORK/Applications/$APP_NAME" && echo "  安装位置签名: 有效 ✅"

echo "[3/7] 从安装位置启动（干净数据目录，脱离开发目录）"
DATA="$WORK/data"
BIN="$WORK/Applications/$APP_NAME/Contents/MacOS/$(plutil -extract CFBundleExecutable raw "$WORK/Applications/$APP_NAME/Contents/Info.plist")"
DXO_DATA_DIR="$DATA" "$BIN" > "$WORK/app-stdout.log" 2>&1 &
PID=$!
sleep 6
kill -0 "$PID" || { echo "  ❌ 进程已退出"; cat "$WORK/app-stdout.log"; exit 1; }
echo "  进程存活 PID=$PID ✅  运行路径: $BIN"
echo "  数据目录内容: $(ls "$DATA" | tr '\n' ' ')"
[ -f "$DATA/dxo_filters.db" ] || { echo "  ❌ 未创建本地数据库"; exit 1; }
lsof -nP -a -p "$PID" -i 2>/dev/null | sed 's/^/  网络: /' || echo "  无对外网络套接字 ✅"

echo "[4/7] 内置 Lightning 整包自包含校验（19 件 SHA 与字节都在二进制内）"
python3 - "$BIN" "$(cd "$PROJECT_DIR/../.." && pwd)/Lightning-L口" <<'PY'
import pathlib, sys
binary = pathlib.Path(sys.argv[1]).read_bytes()
bundle = pathlib.Path(sys.argv[2])
rows = [line.split("  ") for line in (bundle / "SHA256SUMS.txt").read_text().splitlines() if line]
missing_sha, missing_bytes = [], []
for sha, rel in rows:
    if sha.encode() not in binary:
        missing_sha.append(rel)
    if (bundle / rel).read_bytes() not in binary:
        missing_bytes.append(rel)
print(f"  清单 {len(rows)} 件；二进制内缺 SHA: {missing_sha or '无'}；缺字节: {missing_bytes or '无'}")
assert len(rows) == 19 and not missing_sha and not missing_bytes, "内置整包不完整"
PY

echo "[5/7] 主窗口与截图证据"
WINDOWS="$(swift "$SCRIPT_DIR/tools/window-list.swift" "$PID" || true)"
echo "  窗口列表: ${WINDOWS:-（空）}"
WIN_ID="$(echo "$WINDOWS" | awk '$2 >= 1000 && $3 >= 600 {print $1; exit}')"
[ -n "$WIN_ID" ] || { echo "  ❌ 未找到 1280x820 级别主窗口"; exit 1; }
screencapture -o -x -l "$WIN_ID" "$SHOTS_DIR/macos-installed-default.png"
echo "  截图: $SHOTS_DIR/macos-installed-default.png ($(stat -f%z "$SHOTS_DIR/macos-installed-default.png") B)"
python3 - "$SHOTS_DIR/macos-installed-default.png" <<'PY'
import collections, sys
from PIL import Image
im = Image.open(sys.argv[1]).convert("RGB")
px = list(im.resize((120, 77)).getdata())
dark = sum(1 for p in px if max(p) < 60)
colors = len(collections.Counter(px))
print(f"  画面 {im.size[0]}x{im.size[1]}：深色像素占比 {dark/len(px):.2f}，取样色数 {colors}")
assert dark / len(px) > 0.25, "深色界面占比过低，疑似空白窗口"
assert colors > 40, "画面过于单调，疑似未渲染"
PY

echo "[6/7] 覆盖安装（同一路径替换 App，数据目录保持不变）"
# 内容级快照：除 logs/ 外的数据文件（滤镜库 dxo_filters.db、store、cubes、卡备份）
# 逐一记哈希，覆盖安装+再启动后必须逐字节一致。
# 曾用 `[ 哈希一致 ] && echo ✅` 的弱校验：set -e 不拦 AND 列表左部的失败，
# 比较不符时脚本照旧截图、照旧退出 0（独立复核 2026-11-19 确认的假绿缺陷）。
manifest_data() {
  find "$DATA" -type f -not -path "*/logs/*" -print0 | sort -z \
    | while IFS= read -r -d '' f; do
        printf '%s  %s\n' "$(shasum -a 256 "$f" | cut -d' ' -f1)" "${f#"$DATA"/}"
      done
}
kill "$PID"; wait "$PID" 2>/dev/null || true
PID=""
manifest_data > "$WORK/data-manifest-1"
[ -s "$WORK/data-manifest-1" ] || { echo "  ❌ 首次启动后数据目录为空（未生成滤镜库）"; exit 1; }
grep -qx ".*  dxo_filters.db" "$WORK/data-manifest-1" || { echo "  ❌ 首次启动后未生成 dxo_filters.db"; exit 1; }
echo "  首次启动数据快照: $(wc -l < "$WORK/data-manifest-1" | tr -d ' ') 个文件（logs 除外）"
rm -rf "$WORK/Applications/$APP_NAME"
cp -R "$WORK/mnt/$APP_NAME" "$WORK/Applications/$APP_NAME"
codesign --verify --deep --strict "$WORK/Applications/$APP_NAME" && echo "  覆盖后签名: 有效 ✅"
DXO_DATA_DIR="$DATA" "$BIN" > "$WORK/app-stdout-2.log" 2>&1 &
PID=$!
sleep 6
kill -0 "$PID" || { echo "  ❌ 覆盖安装后无法启动"; cat "$WORK/app-stdout-2.log"; exit 1; }
screencapture -o -x -l "$(swift "$SCRIPT_DIR/tools/window-list.swift" "$PID" | awk '$2>=1000{print $1; exit}')" "$SHOTS_DIR/macos-installed-reinstall.png"
kill "$PID"; wait "$PID" 2>/dev/null || true
PID=""
manifest_data > "$WORK/data-manifest-2"
if diff -u "$WORK/data-manifest-1" "$WORK/data-manifest-2" > "$WORK/data-manifest.diff"; then
  echo "  覆盖安装+再启动后本地数据逐字节未变 ✅（滤镜库/参数/备份，logs 除外）"
else
  echo "  ❌ 覆盖安装改动了本地数据（滤镜库/参数/备份），差异如下："
  cat "$WORK/data-manifest.diff"
  exit 1
fi

echo "[7/7] 结束实例并卸载镜像"
if [ -n "$PID" ]; then kill "$PID"; wait "$PID" 2>/dev/null || true; fi
PID=""
hdiutil detach "$WORK/mnt" -force >/dev/null && echo "  已卸载 ✅"
echo "完成 ✅ 安装路径验收通过（工作目录随脚本退出清理，本机未新增第二份 App）"
