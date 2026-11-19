#!/bin/bash
# ============================================================================
# DXO-ONE-XLAB · macOS arm64 发行包（DMG）构建
# ============================================================================
# 唯一发行入口：Tauri 2 bundle（不再手工组装 .app，避免「本地跑手工构建、DMG 里装另一份前端」）。
#
# 用法:   ./dist/build-macos-release.sh
# 产物:   dist/release/DXO-ONE-XLAB-<version>-macOS-<arch>.dmg
#         dist/release/SHA256SUMS.txt
#
# 顺序约束（不可调换）：编译 → 个人路径抹除 → bundle 组装 → codesign → DMG 打包。
# 任何二进制改写必须发生在签名之前，签名后不得再改。
#
# 凭据（只列变量名，本脚本不写入任何值）：
#   APPLE_SIGNING_IDENTITY / APPLE_ID / APPLE_PASSWORD / APPLE_TEAM_ID
# 未提供 APPLE_SIGNING_IDENTITY 时按 tauri.macos.conf.json 的 signingIdentity "-"
# 做 ad-hoc 签名，产物属于「未公证测试包」，不得对外宣称为 Developer ID 正式发行版。
# 提供 APPLE_SIGNING_IDENTITY 则：钥匙串必须真有该身份 → hardenedRuntime 签名 →
# notarytool 提交 DMG 并 staple；缺 APPLE_ID/APPLE_PASSWORD/APPLE_TEAM_ID 直接失败，
# 不允许「签了名但没公证」被误当成正式包。
# ============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
APP_DIR="$PROJECT_DIR/dxo-app"
OUT_DIR="$SCRIPT_DIR/release"
REPO_ROOT="$(cd "$PROJECT_DIR/../.." && pwd)"
TAURI_CLI_VERSION="${TAURI_CLI_VERSION:-2.12.6}"
ARCH="$(uname -m)"
TRIPLE="$ARCH-apple-darwin"
[ "$ARCH" = "arm64" ] && TRIPLE="aarch64-apple-darwin"

VERSION="$(grep -m1 '^version' "$APP_DIR/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')"
DMG_NAME="DXO-ONE-XLAB-${VERSION}-macOS-${ARCH}.dmg"

# 凭据前置校验（fail fast，避免编译 3 分钟后才发现签不了）
if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
  security find-identity -v -p codesigning | grep -qF -- "$APPLE_SIGNING_IDENTITY" || {
    echo "钥匙串中没有 APPLE_SIGNING_IDENTITY 指定的 Developer ID 身份"; exit 1; }
  for v in APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID; do
    [ -n "${!v:-}" ] || { echo "已给 APPLE_SIGNING_IDENTITY 但缺 $v：不允许交付「已签名未公证」的包"; exit 1; }
  done
fi

assert_no_personal_path() { # <file> <标签>
  local hits
  hits="$(strings -a "$1" | grep -c "$HOME/" || true)"
  if [ "$hits" != "0" ]; then
    echo "❌ $2 仍含 $hits 处个人绝对路径（$HOME）"
    return 1
  fi
  echo "  个人路径扫描（$2）: 无 ✅"
}

echo "[1/8] 前置检查 arch=$ARCH triple=$TRIPLE version=$VERSION tauri-cli=$TAURI_CLI_VERSION"
# rust-toolchain.toml pin 1.99.1；本机 stable 通道即 rustc 1.99.1，显式选用避免再下载独立副本。
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
echo "  $(rustc --version) / $(cargo --version) (RUSTUP_TOOLCHAIN=$RUSTUP_TOOLCHAIN)"
rustup target list --installed | grep -qx "$TRIPLE" || { echo "缺少 Rust 目标 $TRIPLE"; exit 1; }
for input in "$REPO_ROOT/Lightning-L口/SHA256SUMS.txt" "$REPO_ROOT/DEPENDENCIES.md" \
             "$REPO_ROOT/LICENSE" "$APP_DIR/ui/index.html" "$APP_DIR/icons/AppIcon.icns"; do
  [ -e "$input" ] || { echo "构建输入缺失: $input"; exit 1; }
done
python3 - "$APP_DIR" "$VERSION" <<'PY'
import json, pathlib, sys
app_dir, version = pathlib.Path(sys.argv[1]), sys.argv[2]
conf = json.loads((app_dir / "tauri.conf.json").read_text())
if conf["version"] != version:
    sys.exit(f"版本不一致: tauri.conf.json={conf['version']} Cargo.toml={version}")
if conf["productName"] != "DXO-ONE-XLAB" or conf["identifier"] != "cn.dxoxlab.luttool":
    sys.exit(f"产品标识被改动: {conf['productName']} / {conf['identifier']}")
print(f"  版本/标识一致: {conf['productName']} {version} {conf['identifier']}")
PY

echo "[2/8] 编译 release 二进制（tauri build --no-bundle，RUSTFLAGS remap）"
export RUSTFLAGS="--remap-path-prefix=$HOME=/user${RUSTFLAGS:+ $RUSTFLAGS}"
( cd "$APP_DIR" && npx --yes "@tauri-apps/cli@$TAURI_CLI_VERSION" build --no-bundle --target "$TRIPLE" )

echo "[3/8] 个人路径抹除（签名前等长替换）"
python3 "$SCRIPT_DIR/scrub-macos-binary.py"

echo "[4/8] Tauri bundle：组装 .app 并签名"
# 有 Developer ID 身份时用 -c 覆盖 conf 里的 ad-hoc 身份；覆盖只影响打包，不改源码
SIGN_CONFIG=""
if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
  SIGN_CONFIG=$(python3 -c 'import json,os;print(json.dumps({"bundle":{"macOS":{"signingIdentity":os.environ["APPLE_SIGNING_IDENTITY"],"hardenedRuntime":True}}}))')
  echo "  使用 Developer ID 签名 + hardenedRuntime"
fi
( cd "$APP_DIR" && npx --yes "@tauri-apps/cli@$TAURI_CLI_VERSION" bundle --bundles app --target "$TRIPLE" ${SIGN_CONFIG:+--config "$SIGN_CONFIG"} )

BUNDLE_DIR="$PROJECT_DIR/target/$TRIPLE/release/bundle"
SRC_APP="$BUNDLE_DIR/macos/DXO-ONE-XLAB.app"
[ -d "$SRC_APP" ] || { echo "bundle 产物缺失: $SRC_APP"; exit 1; }
BIN_NAME="$(basename "$(plutil -extract CFBundleExecutable raw "$SRC_APP/Contents/Info.plist")")"

echo "[5/8] 校验 .app：签名、版本一致性、个人路径"
codesign --verify --deep --strict --verbose=2 "$SRC_APP"
if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
  codesign -dvv "$SRC_APP" 2>&1 | grep -E "Authority|TeamIdentifier|Signature" | sed 's/^/  /'
else
  codesign -dvv "$SRC_APP" 2>&1 | grep -E "flags=|Signature=" | sed 's/^/  签名渠道: /'
fi
[ "$(plutil -extract CFBundleShortVersionString raw "$SRC_APP/Contents/Info.plist")" = "$VERSION" ] || {
  echo "Info.plist CFBundleShortVersionString 与 Cargo.toml 不一致"; exit 1; }
assert_no_personal_path "$SRC_APP/Contents/MacOS/$BIN_NAME" ".app 二进制"

echo "[6/8] 生成 DMG（hdiutil，含 Applications 快捷方式）"
# 不用 Tauri 自带 create-dmg：它依赖 Finder AppleScript 排布窗口，非交互环境会
# 卡在自动化授权直至超时；纯 hdiutil 卷同样是「拖入应用程序」安装。
mkdir -p "$OUT_DIR"
STAGE="$(mktemp -d "${TMPDIR:-/tmp/}dxo-dmg-stage.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT
cp -R "$SRC_APP" "$STAGE/DXO-ONE-XLAB.app"
ln -s /Applications "$STAGE/Applications"
codesign --verify --deep --strict "$STAGE/DXO-ONE-XLAB.app" || { echo "复制后签名失效"; exit 1; }
hdiutil create -verbose -ov -volname "DXO-ONE-XLAB ${VERSION}" -fs HFS+ \
  -srcfolder "$STAGE" -format UDZO -o "$OUT_DIR/$DMG_NAME"

echo "[7/8] 校验 DMG：挂载后复检装箱内容"
MOUNT="$(mktemp -d "${TMPDIR:-/tmp/}dxo-dmg.XXXXXX")"
trap 'hdiutil detach "$MOUNT" -force >/dev/null 2>&1 || true; rm -rf "$MOUNT" "$STAGE"' EXIT
hdiutil attach -nobrowse -readonly -mountpoint "$MOUNT" "$OUT_DIR/$DMG_NAME" >/dev/null
ls -la "$MOUNT" | sed 's/^/  /'
codesign --verify --deep --strict --verbose=2 "$MOUNT/DXO-ONE-XLAB.app"
assert_no_personal_path "$MOUNT/DXO-ONE-XLAB.app/Contents/MacOS/$BIN_NAME" "DMG 内二进制"
hdiutil detach "$MOUNT" -force >/dev/null

# 公证必须在卸载之后、写 SHA 之前：staple 会改写 DMG 本体，先写 SHA 就白写了
if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
  echo "  公证：notarytool submit + stapler staple"
  : "${APPLE_ID:?缺少 APPLE_ID，无法公证（不允许交付「已签名未公证」的包）}"
  : "${APPLE_PASSWORD:?缺少 APPLE_PASSWORD（App 专用密码），无法公证}"
  : "${APPLE_TEAM_ID:?缺少 APPLE_TEAM_ID，无法公证}"
  xcrun notarytool submit "$OUT_DIR/$DMG_NAME" --apple-id "$APPLE_ID" \
    --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" --wait
  xcrun stapler staple "$OUT_DIR/$DMG_NAME"
  xcrun stapler validate "$OUT_DIR/$DMG_NAME"
  spctl --assess --type open --context path:="$OUT_DIR/$DMG_NAME" -vv 2>&1 | sed 's/^/  Gatekeeper: /'
else
  echo "  未公证（本机无 Developer ID 凭据）：本包只能作为未签名测试包审查"
fi

echo "[8/8] 写 SHA 并清理构建侧第二份 App 与 DMG 工作区（本机不保留可启动的第二份 App）"
( cd "$OUT_DIR" && rm -f SHA256SUMS.txt && shasum -a 256 "$DMG_NAME" >> SHA256SUMS.txt )
rm -rf "$BUNDLE_DIR/macos" "$BUNDLE_DIR/dmg"
( cd "$OUT_DIR" && shasum -a 256 -c SHA256SUMS.txt )
ls -la "$OUT_DIR"
echo "完成 ✅  $OUT_DIR/$DMG_NAME"
