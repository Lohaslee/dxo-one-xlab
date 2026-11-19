#!/bin/bash
# 从源码验证当前 Tauri App；所有写入测试使用临时目录，不运行 ignored 硬件测试。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
REPO_DIR="$(cd "$PROJECT_DIR/../.." && pwd)"

cd "$REPO_DIR/Lightning-L口"
shasum -a 256 -c SHA256SUMS.txt
cd "$PROJECT_DIR"

node --test dxo-app/tests/ui-state.test.cjs
cargo test --locked -p lut-core -p lut-persist -p lut-camera -p dxo-app
bash "$SCRIPT_DIR/build-app.sh"
