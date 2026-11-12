#!/bin/bash
# 自定义置入滤镜流程 · 转换壳
# 用法：把 .cube 文件拖到本文件上；或双击后按提示粘贴路径
DIR="$(cd "$(dirname "$0")" && pwd)"
PY="/Users/a123/.workbuddy/binaries/python/envs/default/bin/python3"
[ -x "$PY" ] || PY="$(command -v python3)"
echo "===== DxO ONE 自定义滤镜转换 ====="
echo
if [ $# -eq 0 ]; then
  echo "把 .cube 文件拖到本文件上即可；也可以现在粘贴一个路径："
  read -r P
  set -- "$P"
fi
for f in "$@"; do
  if [ ! -f "$f" ]; then echo "跳过（找不到）：$f"; continue; fi
  echo "--- $(basename "$f") ---"
  "$PY" "$DIR/cube2dxo.py" "$f" --tone neutral
  echo
done
echo "输出目录：<仓库根>/成品/lut/"
echo "每个 LUT 生成 <名字>_neutral.bin（整表 17,536 B）与 _s0..s4.bin（5 个槽载荷）"
echo "置入方法见同目录 README.md"
echo
echo "按回车关闭窗口"
read -r _
