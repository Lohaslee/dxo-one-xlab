# USB-C 版触屏补丁（林达 v26）—— 不是滤镜补丁

> **这是林达 (@PENTAX_FA43mm) 的 v26 触屏补丁原包**，供 **USB-C 原生版 DxO ONE**
> 恢复/安装触屏功能。本仓库自己的滤镜补丁（`patch/`，Build 66）**只支持
> iOS / Lightning 机型**，两个版本的固件内存地址完全不同，**互相不兼容、
> 不能混装**。

## 你该用哪个

| 相机接口 | 用什么 |
|---|---|
| Lightning（iPhone 老接口） | 本仓库 `patch/` 滤镜补丁（Build 66，含触屏功能） |
| USB-C | **本目录**的 v26 触屏补丁。滤镜补丁请勿使用 |

不确定自己是哪个？看相机连接电脑/手机的线：老 Apple 口 = Lightning，Type-C 口 = USB-C。

## 安装（全部复制到卡根）

把本目录 **37 件全部**复制到 TF 卡根目录（和 `DCIM` 同级）：

```
autoexec.ash  touch_prefs.bin  p01.bin … p35.bin
```

校验（可选）：`shasum -a 256 -c SHA256SUMS.txt`

- `touch_prefs.bin`：卡上**已有就不要覆盖**（那是相机自己的设置记忆）；
- 装回开机，**等约 3 秒**再触屏。

## 卸载 / 恢复原版

删除卡根的 `autoexec.ash` 和 `p*.bin` 即可（`touch_prefs.bin` 保留）。
补丁只在开机时载入内存，不刷固件。

## 来源与许可

- 作者：**林达 (@PENTAX_FA43mm)**，v26；iOS 移植版作者 Eli（本仓库 `deps/` 的来源，与本目录无关）
- 来源包：`DxO_ONE_USB-C_v26_使用文件.zip`（发布者持有），逐字节未改
- 许可：未见明确许可声明，也未禁止再分发；权利人可随时要求移除，联系方式见仓库根 README
- 本目录内容**不属于**本仓库 LICENSE 的授权范围，使用风险自负
