# Lightning / L口 版 —— DxO ONE 机内滤镜补丁（Build 72）

> **适用：iOS / Lightning 接口、固件 `3.2.0 (ecef5ef809)` 的 DxO ONE。**
> 本目录是一个**整包**：把里面的内容**全部复制到 TF 卡根目录**（和 `DCIM` 同级）即可使用，
> 无需再分别拼补丁与依赖。USB-C 原生版请用仓库的 [USB-C-C口/](../USB-C-C口/README.md)，
> **两套固件内存地址完全不同，互不兼容、禁止混装。**

## 你该用哪个

| 相机接口 | 用什么 |
|---|---|
| Lightning（iPhone 老接口） | **本目录**（Build 72 滤镜补丁，含触屏运行时依赖） |
| USB-C | [USB-C-C口/](../USB-C-C口/README.md)（林达 v26 触屏补丁，无滤镜功能） |

不确定自己是哪个？看相机连接电脑/手机的线：老 Apple 口 = Lightning，Type-C 口 = USB-C。

## 这一版做了什么

- 相机菜单新增第六页，**十一档滤镜循环**（上下切滤镜、左右翻页），含第 11 档 INSTA
- **滤镜记忆（Build 72）**：切档成功即把上次档位写进卡根 `ph_last.bin`（8 字节，相机自建），
  下次开机自动恢复上次关机前所用的滤镜；文件缺失/损坏/越界时自动回退预装档 GOLD200
- 自制等宽像素字体，滤镜名居中显示；切档后预览立即刷新
- 保留原厂曝光与拨盘分派；横滑预览不改拍摄设置；半按直接预览；关机无残字

实机验收用例 0–7 全部通过（2026-11-17，用户亲自验收）。详见仓库根 [README.md](../README.md)。

## 安装（全部复制到卡根）

把本目录以下内容**全部**复制到 TF 卡根目录（和 `DCIM` 同级）：

```
autoexec.ash  ph_probe.ash  ph_menu.bin  touch_prefs.bin
dxot/         （a.bin  b.bin  m01.ash  m02.ash）
DXO_FILTERS/  （11 个 lut_*.bin，含 lut_insta.bin）
PH_DIAG/      （空目录，里面只有一个 .gitkeep 占位）
```

校验（可选，在本目录内执行）：`shasum -a 256 -c SHA256SUMS.txt`（19 件）

**红线与注意**

- `touch_prefs.bin`：卡上**已有就不要覆盖**（那是相机自己的档位/曝光/白平衡/MF 距离记忆）；
  卡根没有才复制本目录这份。
- `dxot/`：触屏运行时依赖（第三方，见 [DEPENDENCIES.md](../DEPENDENCIES.md)），整目录复制到卡根。
- **`PH_DIAG/` 目录必须存在**：相机端 ash 没有 `mkdir`，诊断文件要往里写；目录不在时采集静默失败
  （菜单与滤镜仍正常）。在相机里格式化过卡后需重新把这个空目录拷进去。
- 绝不删除或改写 `DCIM/`；不要混用不同 Build 的 `ph_*` 信标。
- `ph_last.bin`（滤镜记忆）由相机运行时自建，**部署时不需要、也不要手工预置或修改**；
  想清除记忆回预装档 GOLD200，删除卡根 `ph_last.bin` 即可。
- 逐文件回读 SHA-256 与清单比对，正常弹出卷后再拔卡；装回开机**等约 3 秒**再触屏。

> **正在用其他触屏补丁（如林达 USB-C v26）？先停一下。** 两套都以卡根 `autoexec.ash` 为开机入口，
> 复制本套会覆盖它，你的触屏补丁会立刻失效，而本套在 USB-C 固件上又装不上。USB-C 用户请用
> [USB-C-C口/](../USB-C-C口/README.md)。

## 换成自己的滤镜

菜单是「登记制」：`ph_menu.bin` 只认登记过的文件名并校验内容指纹。替换某档 = 同名覆盖
`DXO_FILTERS/lut_<档>.bin` **＋ 重登记指纹**（缺一不可）；新增/删除档位用管理工具重建菜单。
完整流程、验证边界与 AI 提示词见 [自定义置入滤镜流程/](../自定义置入滤镜流程/README.md)。
**`ph_menu.bin` 只能用工具生成，不手改二进制。**

## 卸载 / 恢复原版

补丁只在开机时由卡根 `autoexec.ash` 载入运行内存，**不刷固件**。

| 方案 | 做法 |
|---|---|
| A. 停用（推荐，可逆） | 把卡根 `autoexec.ash` 改名为 `autoexec.ash.disabled`，其余原样保留 |
| B. 彻底清理 | 删除卡根 `autoexec.ash`、`ph_probe.ash`、`ph_menu.bin`、`PH_DIAG/`、`DXO_FILTERS/`、`dxot/` |

两种方案都**必须保留** `DCIM/` 与 `touch_prefs.bin`。

## 来源与许可

- 滤镜补丁程序与二进制（`autoexec.ash`、`ph_probe.ash`、`ph_menu.bin`、`DXO_FILTERS/`）：
  本仓库原创，**All Rights Reserved**，允许个人在自己设备上非商业使用，禁止再分发/商用/公开衍生，
  见 [LICENSE](../LICENSE)。
- `dxot/` 与 `touch_prefs.bin`：**第三方运行时依赖**，来自林达 (@PENTAX_FA43mm) v26 触屏补丁与
  Eli 的 iOS 移植版，未见明确许可证，**不属于本仓库 LICENSE 授权范围**，部署前必读
  [DEPENDENCIES.md](../DEPENDENCIES.md)。
- 本项目与 DxO Labs 无任何关联，未获其授权或认可。使用风险自负。