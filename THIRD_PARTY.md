# 第三方材料与版权声明

本仓库发布的是**独立制作的机内滤镜补丁**。下列第三方材料版权归各自权利人所有，
本仓库不主张其权利。请在使用前阅读本节。

---

## 1. DxO ONE 固件与官方软件 —— 不包含

| 材料 | 权利人 | 本仓库状态 |
|---|---|---|
| 固件 3.2.0 (`ecef5ef809`) 完整镜像 | DxO Labs | 不包含 |
| `section_0..5` 分区镜像、ROMFS 提取内容 | DxO Labs | 不包含 |
| 官方安装包、安装器 `.exe`、`p*.bin` | DxO Labs | 不包含 |
| AmbaShell 命令（`t dxo`、`writel`、`alarm`、`savebin` 等） | DxO Labs / Ambarella | 仅在脚本中按命令名调用 |

本仓库不提供固件提取工具、镜像或下载链接。补丁中出现的内存地址、
符号偏移与命令名，属于研究记录性质的引用。

## 2. 第三方 v26 触屏补丁 —— 不包含，但**运行时依赖**

`autoexec.ash` 会调用卡上的 `dxot/m01.ash`、`dxot/m02.ash`、`dxot/a.bin`、
`dxot/b.bin`，并读取 `touch_prefs.bin`。这些文件属于第三方 v26 触屏补丁
（USB-C / Lightning 版）及其配套资源，**整包不在本仓库内**，本仓库也不提供
获取渠道、不复制、不改编、不声称对其拥有任何权利。

未安装该环境时本补丁无法启动。请自行从合法来源获取，并自行确认原作者的
授权条件；本仓库对第三方补丁的可用性、安全性与授权不作任何承诺。

## 3. 色彩查找表（CC 表）—— **包含 10 个二进制**

`patch/DXO_FILTERS/` 下的 10 个 `.bin` 是机内 CC 表打包文件（每张 87,720 B，
含 5 个 17,536 B 槽位）。

| 档位 | 文件 | 来源性质 |
|---|---|---|
| ORIGINAL | `lut_orig.bin` | 透传档，保留源影调；非原厂 CC 表副本 |
| VELVIA / GOLD200 / PORTRA / HP5 | `lut_vel.bin` 等 | 由第三方 `.cube` 素材转换而来 |
| RICOH POSI / RICOH HIBW | `lut_posi.bin` / `lut_hibw.bin` | 由第三方 Ricoh GR II 风格 `.cube` 转换而来 |
| FUYUNO / KYUSHU / SATOBI | `lut_fuyu.bin` 等 | 由第三方 Pentax K12 风格 `.cube` 转换而来 |

**本仓库不包含任何 `.cube` 源文件**，只包含转换后的机内表。上述第三方 LUT
素材的许可条件各不相同，通常限制再分发；维护者**不主张**这些素材或其派生表的
权利，也**不承诺**本仓库中的派生表可自由再分发。使用者请自行取得有合法许可的
原始素材，并自行承担相应法域下的合规责任。

> 如果你是上述 LUT 素材的权利人，认为本仓库中的派生表超出授权范围，
> 请通过 GitHub Issues 联系维护者，我们会核实并及时移除相关内容。

## 4. 补丁自身

`patch/autoexec.ash`、`patch/ph_probe.ash`、`patch/ph_menu.bin` 及菜单融合逻辑
为维护者原创，适用 [LICENSE](LICENSE) 第 1 节（All Rights Reserved）。

## 5. 商标

「DxO」「DxO ONE」及相关标识为各自权利人的商标或注册商标。本仓库仅为描述
适配对象而作指示性使用，不表示任何背书、关联或授权关系。

---

_本文件为仓库文档，适用 CC BY-NC 4.0；见 [LICENSE](LICENSE)_
