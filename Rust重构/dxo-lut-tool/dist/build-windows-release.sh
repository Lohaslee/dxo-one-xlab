#!/usr/bin/env bash
# ============================================================================
# DXO-ONE-XLAB · Windows x64 发行包（NSIS EXE）构建
# ============================================================================
# 必须在 Windows x64 + MSVC 上原生运行（本机 Mac 交叉构建不能证明 Windows 可用）。
# 前置：Rust 稳定工具链 + x86_64-pc-windows-msvc、Visual Studio Build Tools（含 link.exe）、
#       Node.js（npx 拉取 @tauri-apps/cli）。
#
# 用法:   bash dist/build-windows-release.sh          # Git Bash / MSYS
# 产物:   dist/release/DXO-ONE-XLAB-<version>-Windows-x64-Setup.exe
#         dist/release/SHA256SUMS.txt
#
# 顺序约束：编译 → bundle（NSIS 打包）→ 代码签名（有凭据时）→ 校验。
# 凭据（只列变量名）：TAURI_WINDOWS_SIGNTOOL 不需要；签名走 Tauri windows.certificateThumbprint
#   + 系统证书，或 TAURI_SIGNING_PRIVATE_KEY（App 更新用，本包未启用）。
# 无签名凭据时产物保持未签名，脚本如实打印 Authenticode 状态，不谎称「正式可信发行版」。
# ============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
APP_DIR="$PROJECT_DIR/dxo-app"
OUT_DIR="$SCRIPT_DIR/release"
REPO_ROOT="$(cd "$PROJECT_DIR/../.." && pwd)"
TAURI_CLI_VERSION="${TAURI_CLI_VERSION:-2.12.6}"

VERSION="$(grep -m1 '^version' "$APP_DIR/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')"
EXE_NAME="DXO-ONE-XLAB-${VERSION}-Windows-x64-Setup.exe"
cd "$PROJECT_DIR"

echo "[1/5] 前置检查"
[ "$(uname -s)" = "Windows_NT" ] || { echo "本脚本必须在 Windows 上运行（当前 $(uname -s)）"; exit 1; }
command -v rustc >/dev/null || { echo "缺少 Rust"; exit 1; }
rustc -vV | grep -q 'host: x86_64-pc-windows-msvc' || { echo "Rust host 不是 x86_64-pc-windows-msvc"; exit 1; }
for input in "$REPO_ROOT/Lightning-L口/SHA256SUMS.txt" "$REPO_ROOT/DEPENDENCIES.md" \
             "$APP_DIR/ui/index.html" "$APP_DIR/icons/icon.ico"; do
  [ -e "$input" ] || { echo "构建输入缺失: $input"; exit 1; }
done
echo "  $(rustc --version) / $(cargo --version)"

echo "[2/5] 构建 + NSIS 打包（回归测试由 CI / dist/verify-source.sh 单独执行）"
export RUSTFLAGS="--remap-path-prefix=$USERPROFILE=/user${RUSTFLAGS:+ $RUSTFLAGS}"
# 有代码签名证书时用 --config 覆盖（值只从环境读取，不落文件）；无凭据保持未签名
SIGN_CONFIG=""
if [ -n "${TAURI_WINDOWS_CERT_THUMBPRINT:-}" ]; then
  case "$TAURI_WINDOWS_CERT_THUMBPRINT" in
    *[!0-9A-Fa-f]*) echo "TAURI_WINDOWS_CERT_THUMBPRINT 应为十六进制指纹，拒接注入"; exit 1 ;;
  esac
  SIGN_CONFIG="{\"bundle\":{\"windows\":{\"certificateThumbprint\":\"$TAURI_WINDOWS_CERT_THUMBPRINT\",\"digestAlgorithm\":\"sha256\",\"timestampUrl\":\"http://timestamp.digicert.com\"}}}"
  echo "  已配置证书指纹 → 走 Tauri 签名 + 时间戳"
fi
( cd "$APP_DIR" && npx --yes "@tauri-apps/cli@$TAURI_CLI_VERSION" build --bundles nsis ${SIGN_CONFIG:+--config "$SIGN_CONFIG"} )

SRC_DIR="$PROJECT_DIR/target/release/bundle/nsis"
SRC_EXE="$(ls -1 "$SRC_DIR"/*-setup.exe | head -1)"
[ -f "$SRC_EXE" ] || { echo "NSIS 产物缺失: $SRC_DIR"; exit 1; }

echo "[3/5] 校验安装包元信息与签名状态"
WIN_EXE="$(cygpath -w "$SRC_EXE")"
powershell.exe -NoProfile -Command \
  "Get-Item -LiteralPath '$WIN_EXE' | Select-Object -ExpandProperty VersionInfo | Format-List FileVersion,ProductName,Company; Get-AuthenticodeSignature -LiteralPath '$WIN_EXE' | Format-List Status,StatusMessage" | sed 's/^/  /' || true
if [ -n "${TAURI_WINDOWS_CERT_THUMBPRINT:-}" ]; then
  STATUS="$(powershell.exe -NoProfile -Command "Get-AuthenticodeSignature -LiteralPath '$WIN_EXE' | Select-Object -ExpandProperty Status" | tr -d '\r')"
  [ "$STATUS" = "Valid" ] || { echo "已提供签名凭据但 Authenticode 状态为「$STATUS」，不能作为正式发行包"; exit 1; }
  echo "  Authenticode: Valid ✅"
fi

echo "[4/5] 归位产物 $OUT_DIR/$EXE_NAME"
mkdir -p "$OUT_DIR"
cp -f "$SRC_EXE" "$OUT_DIR/$EXE_NAME"

echo "[5/5] SHA256 与清理"
( cd "$OUT_DIR" && rm -f SHA256SUMS.txt && sha256sum "$EXE_NAME" > SHA256SUMS.txt && sha256sum -c SHA256SUMS.txt )
rm -rf "$SRC_DIR"
ls -la "$OUT_DIR"
echo "完成 ✅  $OUT_DIR/$EXE_NAME"
echo "注意：未签名时 SmartScreen 仍可能提示；WebView2 运行时缺失时安装过程需要联网。"
