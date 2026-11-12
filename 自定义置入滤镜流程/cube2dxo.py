#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
DxO ONE 机内 CC 3D LUT 格式转换
================================
相机格式（ROMFS 字节结构与单轴响应支持下述解码，硬件采样仍待独立验证）：
  · 文件 17,536 字节 = 0x4480
  · 0x0000..0x007F  128 字节文件头（u32 magic=1, u32 version=1，其余 0）
  · 0x0080..0x4080  16,384 字节 = 4096 个节点 × 4 字节
      每节点：bits 0-9 = R，bits 10-19 = G，bits 20-29 = B（均 0..1023）
      索引 = R_idx*256 + G_idx*16 + B_idx   （B 变化最快）
      按均匀网格 idx/15 解码；硬件实际输入编码及节点定位仍待独立验证
  · 0x4080..0x4480  1024 字节，两段 256 项 u16 曲线（实测改动无效果，原样继承）

当前采用的实验性复合：
      新表(输入) = 目标LUT( 原厂表(输入) )
原厂 CC 表承担影调与色彩处理；其输出是否等同目标 .cube 要求的输入编码，
必须由素材说明和硬件链路另行确认。文件长度与解码回写自检不能证明输入域正确。
neutral 是主动抵消 LUT 灰轴影调的风格选项，不是 Log/色域转换。
"""
import os
import re
import struct
from dataclasses import dataclass
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
STOCK = os.path.join(ROOT, "固件镜像", "romfs", "section_2", "Imx183_Cc_Still1.bin")

N = 16                      # 相机立方体边长
BODY_OFF = 0x80
BODY_LEN = N * N * N * 4    # 16384
TABLE_LEN = 17536
MAXV = 1023


# ---------------------------------------------------------------- 相机表读写
def decode_table(path):
    """读相机表 -> (header128, cube[16,16,16,3] 浮点0..1, tail1024)"""
    with open(path, "rb") as f:
        d = f.read()
    if len(d) != TABLE_LEN:
        raise ValueError(f"{path} 长度 {len(d)} 不是 {TABLE_LEN}")
    if d[:BODY_OFF] != struct.pack("<II", 1, 1) + bytes(BODY_OFF - 8):
        raise ValueError(f"{path} 不是已支持的 CC 文件头")
    header = d[:BODY_OFF]
    tail = d[BODY_OFF + BODY_LEN:]
    cube = np.zeros((N, N, N, 3), dtype=np.float32)
    for r in range(N):
        for g in range(N):
            for b in range(N):
                idx = r * 256 + g * 16 + b
                w = struct.unpack_from("<I", d, BODY_OFF + 4 * idx)[0]
                cube[r, g, b, 0] = (w & 0x3FF) / MAXV
                cube[r, g, b, 1] = ((w >> 10) & 0x3FF) / MAXV
                cube[r, g, b, 2] = ((w >> 20) & 0x3FF) / MAXV
    return header, cube, tail


def encode_table(header, cube, tail):
    cube = np.asarray(cube)
    if header != struct.pack("<II", 1, 1) + bytes(BODY_OFF - 8) or len(tail) != 1024:
        raise ValueError("不支持的 CC 文件头或尾部长度")
    if cube.shape != (N, N, N, 3) or not np.isfinite(cube).all():
        raise ValueError("CC 节点形状错误或包含 NaN/Inf")
    out = bytearray()
    out += header
    for r in range(N):
        for g in range(N):
            for b in range(N):
                v = np.clip(cube[r, g, b], 0.0, 1.0)
                q = np.round(v * MAXV).astype(np.int64)
                w = int(q[0]) | (int(q[1]) << 10) | (int(q[2]) << 20)
                out += struct.pack("<I", w)
    out += tail
    assert len(out) == TABLE_LEN, len(out)
    return bytes(out)


# ---------------------------------------------------------------- .cube 读写
@dataclass
class CubeLUT:
    values: np.ndarray
    domain_min: np.ndarray
    domain_max: np.ndarray


def read_cube(path):
    """读 .cube -> (size, CubeLUT)，DOMAIN 定义输入坐标而非输出值。"""
    size = None
    domain_min = np.zeros(3, np.float32)
    domain_max = np.ones(3, np.float32)
    seen, vals = set(), []
    with open(path, "r", encoding="utf-8-sig") as f:
        for lineno, line in enumerate(f, 1):
            s = line.split("#", 1)[0].strip()
            if not s:
                continue
            parts = s.split()
            key = parts[0].upper()
            if key in ("TITLE", "LUT_3D_SIZE", "DOMAIN_MIN", "DOMAIN_MAX", "LUT_3D_INPUT_RANGE"):
                if vals or key in seen:
                    raise ValueError(f"第 {lineno} 行：重复或位于数据后的头字段 {key}")
                seen.add(key)
                if key == "TITLE":
                    if len(parts) < 2:
                        raise ValueError(f"第 {lineno} 行：TITLE 为空")
                    continue
                if key == "LUT_3D_SIZE":
                    if len(parts) != 2:
                        raise ValueError("LUT_3D_SIZE 必须有一个整数")
                    size = int(parts[1])
                    if size < 2:
                        raise ValueError("LUT_3D_SIZE 必须至少为 2")
                    continue
                count = 2 if key == "LUT_3D_INPUT_RANGE" else 3
                if len(parts) != count + 1:
                    raise ValueError(f"{key} 需要 {count} 个数值")
                v = np.asarray([float(x) for x in parts[1:]], dtype=np.float32)
                if not np.isfinite(v).all():
                    raise ValueError(f"{key} 包含 NaN/Inf")
                if key == "LUT_3D_INPUT_RANGE":
                    domain_min, domain_max = np.full(3, v[0]), np.full(3, v[1])
                elif key == "DOMAIN_MIN":
                    domain_min = v
                else:
                    domain_max = v
                continue
            if key.startswith("LUT_"):
                raise ValueError(f"不支持的 LUT 头字段 {key}")
            if size is None or len(parts) != 3:
                raise ValueError(f"第 {lineno} 行：缺少尺寸或不是 RGB 数据")
            try:
                vals.append([float(x) for x in parts])
            except ValueError as exc:
                raise ValueError(f"第 {lineno} 行：非法 RGB 数据") from exc
    if size is None:
        raise ValueError("没找到 LUT_3D_SIZE")
    if "LUT_3D_INPUT_RANGE" in seen and seen.intersection(("DOMAIN_MIN", "DOMAIN_MAX")):
        raise ValueError("不可混用 LUT_3D_INPUT_RANGE 与 DOMAIN_MIN/MAX")
    if not np.all(domain_max > domain_min):
        raise ValueError("输入域上限必须大于下限")
    arr = np.asarray(vals, dtype=np.float32)
    if arr.shape != (size ** 3, 3) or not np.isfinite(arr).all():
        raise ValueError(f"RGB 数据须为 {size}^3 行有限数值")
    # 标准顺序 index=b*size²+g*size+r；保留输出数值，最终硬件编码时再限幅。
    lut = np.transpose(arr.reshape(size, size, size, 3), (2, 1, 0, 3))
    return size, CubeLUT(lut, domain_min, domain_max)


def sample(lut, coords):
    """三线性插值；CubeLUT 使用声明的输入域，普通数组使用归一化坐标。"""
    if isinstance(lut, CubeLUT):
        coords = (np.asarray(coords) - lut.domain_min) / (lut.domain_max - lut.domain_min)
        lut = lut.values
    s = lut.shape[0]
    x = np.clip(coords, 0.0, 1.0) * (s - 1)
    i0 = np.floor(x).astype(np.int32)
    i1 = np.minimum(i0 + 1, s - 1)
    f = x - i0
    r0, g0, b0 = i0[..., 0], i0[..., 1], i0[..., 2]
    r1, g1, b1 = i1[..., 0], i1[..., 1], i1[..., 2]
    fr, fg, fb = f[..., 0:1], f[..., 1:2], f[..., 2:3]

    def at(r, g, b):
        return lut[r, g, b]

    c00 = at(r0, g0, b0) * (1 - fb) + at(r0, g0, b1) * fb
    c01 = at(r0, g1, b0) * (1 - fb) + at(r0, g1, b1) * fb
    c10 = at(r1, g0, b0) * (1 - fb) + at(r1, g0, b1) * fb
    c11 = at(r1, g1, b0) * (1 - fb) + at(r1, g1, b1) * fb
    c0 = c00 * (1 - fg) + c01 * fg
    c1 = c10 * (1 - fg) + c11 * fg
    return c0 * (1 - fr) + c1 * fr


# ---------------------------------------------------------------- 主流程
def build_grid():
    """相机 16³ 均匀节点坐标假设，不据此断言硬件输入为线性光。"""
    ax = np.linspace(0.0, 1.0, N, dtype=np.float32)
    r, g, b = np.meshgrid(ax, ax, ax, indexing="ij")
    return np.stack([r, g, b], axis=-1)          # [r][g][b][3]


def neutral_inverse(lut):
    """算出 LUT 中性轴响应的反函数表：给定目标亮度 t，返回应喂入的 x。
    这是有损的影调取消，不是 Log 转换，也不保证保持饱和度或色相。
    非默认域须有共同灰轴输入区间；超出可达亮度时反查会饱和。"""
    if isinstance(lut, CubeLUT):
        lo, hi = float(lut.domain_min.max()), float(lut.domain_max.min())
        if lo >= hi:
            raise ValueError("输入域没有共同灰轴区间，不能使用 neutral")
        size = lut.values.shape[0]
    else:
        lo, hi, size = 0.0, 1.0, lut.shape[0]
    ax = np.linspace(lo, hi, size, dtype=np.float32)
    g = np.stack([ax, ax, ax], axis=-1)
    r = sample(lut, g)
    y = 0.299 * r[:, 0] + 0.587 * r[:, 1] + 0.114 * r[:, 2]
    # 强制单调，便于反查
    for i in range(1, len(y)):
        if y[i] <= y[i - 1]:
            y[i] = y[i - 1] + 1e-6
    ti = np.linspace(0.0, 1.0, 1025, dtype=np.float32)
    inv = np.interp(ti, y, ax).astype(np.float32)
    return ti, inv


def compose(lut, stock_cube, tone="neutral", strength=1.0, desaturate=False):
    """新立方体 = 目标LUT( 原厂表(输入) )
    tone: pass  直接用（保留 LUT 原始影调，通常偏暗）
          neutral  逐通道预补偿灰轴，主动抵消影调，彩色响应亦可能改变
          blend    neutral 与 pass 之间线性混合（strength 0..1，1=完全 neutral）
    """
    grid = build_grid()
    mid = sample(stock_cube, grid)                # 原厂 CC 输出，实际编码仍待独立确认
    if desaturate:
        y = (0.299 * mid[..., 0] + 0.587 * mid[..., 1] + 0.114 * mid[..., 2])[..., None]
        mid = np.repeat(y, 3, axis=-1)
    if lut is None:
        return mid                                 # 恒等测试
    if tone in ("neutral", "blend"):
        ti, inv = neutral_inverse(lut)
        pre = np.interp(mid, ti, inv).astype(np.float32)
        w = 1.0 if tone == "neutral" else float(strength)
        fed = mid * (1.0 - w) + pre * w
    else:
        fed = mid
    return sample(lut, fed)


def write_table(table, out_path, slot_base=0x092D72E8, slot_size=0x4480):
    """写出整表 + slot0 载荷（l0）+ 5 槽载荷（s0..s4）"""
    open(out_path, "wb").write(table)
    base, _ = os.path.splitext(out_path)
    for i in range(5):
        with open(f"{base}_s{i}.bin", "wb") as f:
            f.write(struct.pack("<II", slot_base + i * slot_size, len(table)))
            f.write(table)
    # 槽 0 单槽载荷仅供实验；不能保证覆盖所有 ISO，完整切换使用五槽载荷。
    with open(f"{base}_slot0.bin", "wb") as f:
        f.write(struct.pack("<II", slot_base, len(table)))
        f.write(table)
    return out_path


if __name__ == "__main__":
    import argparse
    ap = argparse.ArgumentParser(description="把 .cube LUT 转成 DxO ONE 机内 CC 表")
    ap.add_argument("cube", nargs="?", help=".cube 文件路径；省略则只做恒等自检")
    ap.add_argument("--tone", default="neutral", choices=["pass", "neutral", "blend"],
                    help="影调处理：pass 原样 / neutral 中灰归一（默认）/ blend 混合")
    ap.add_argument("--strength", type=float, default=0.65,
                    help="tone=blend 时 neutral 的权重 0..1（默认 0.65）")
    ap.add_argument("--bw", action="store_true", help="转成黑白（去色后再套 LUT）")
    ap.add_argument("--name", default=None, help="输出文件名（不含扩展名）")
    ap.add_argument("--outdir", default=None, help="输出目录，默认 成品/lut")
    args = ap.parse_args()

    header, stock_cube, tail = decode_table(STOCK)

    # --- 自检：恒等复合必须还原出原厂表 ---
    ident = encode_table(header, compose(None, stock_cube), tail)
    ref = open(STOCK, "rb").read()
    body_same = sum(1 for a, b in zip(ident[0x80:0x4080], ref[0x80:0x4080]) if a == b)
    print(f"格式自检：LUT 区还原 {body_same}/{BODY_LEN} ({body_same/BODY_LEN*100:.2f}%)")
    assert body_same == BODY_LEN, "格式理解有误，中止"

    if not args.cube:
        raise SystemExit(0)

    name = args.name or os.path.splitext(os.path.basename(args.cube))[0]
    name = re.sub(r"[^0-9A-Za-z._-]+", "_", name).strip("_")   # FAT32 上避免空格/中文
    size, lut = read_cube(args.cube)
    print("警示：尚未独立确认相机 CC 输出与源 LUT 输入编码一致；neutral 不是 Log 转换。")
    if args.tone in ("neutral", "blend"):
        print("警示：neutral/blend 会主动抵消灰轴影调，并可能改变彩色响应及压平端点。")
    new = encode_table(header,
                       compose(lut, stock_cube, tone=args.tone,
                               strength=args.strength, desaturate=args.bw),
                       tail)
    outdir = args.outdir or os.path.join(ROOT, "成品", "lut")
    os.makedirs(outdir, exist_ok=True)
    suffix = "" if args.tone == "pass" else f"_{args.tone}"
    if args.bw:
        suffix += "_BW"
    out = os.path.join(outdir, f"{name}{suffix}.bin")
    write_table(new, out)
    print(f"读入 {name}  LUT_3D_SIZE={size}")
    print(f"已生成 {out}  (+5 槽载荷 + slot0 载荷)")
