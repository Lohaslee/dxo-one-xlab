# DXO-ONE-XLAB 桌面源码 · 0.1.0

当前 App 使用 **Tauri 2 + Rust + 静态 HTML**。本目录包含完整构建输入和测试；无需原开发机目录、用户数据库、原厂固件镜像或私有工具脚本。

## 支持范围

- macOS：提供 Apple Silicon（arm64）DMG，最低系统版本声明为 11.0，已在 macOS 26.1 实测。Intel Mac 和更早系统未验证。
- Windows：提供 x64 NSIS 安装程序，已在 Windows 11 GitHub 托管 runner 验证安装、启动、同包重装和卸载；Windows 10 与交互式桌面体验未验证。卷探测、路径及刷盘已有 Windows 分支，真实读卡器与相机卡写入仍待验收。
- 当前为 **0.1.0 测试包**：Mac 使用 ad-hoc 签名、未公证；Windows 未签名。Linux 不在本次发布范围。
- 软件预览、保存及写卡复用同源 CC 表；不等同于已经完成相机实拍最终色彩一致性验证。

## 安装与升级

两包由提交 `13b8c82` 的[同一次 CI](https://github.com/Lohaslee/dxo-one-xlab/actions/runs/44521929054)构建，已上传到 [GitHub Releases](https://github.com/Lohaslee/dxo-one-xlab/releases/tag/v0.1.0)。按系统下载 DMG 或 Setup.exe，同时下载 `SHA256SUMS.txt`，核对其中对应文件的值。Release 另附对应源码包和 `INSTALL.md`；仓库不跟踪安装包二进制。

macOS 校验：

```bash
shasum -a 256 DXO-ONE-XLAB-0.1.0-macOS-arm64.dmg
```

将结果与清单中的对应条目核对。Windows PowerShell 校验：

```powershell
Get-FileHash .\DXO-ONE-XLAB-0.1.0-Windows-x64-Setup.exe -Algorithm SHA256
```

- **Mac 安装**：打开 DMG，将 App 拖入 Applications，再从“应用程序”打开。若 Gatekeeper 拦截，在确认来源和哈希后，到“系统设置 → 隐私与安全性”检查“仍要打开”选项；是否允许取决于系统及管理策略，无需关闭全局安全保护。
- **Windows 安装**：运行 Setup.exe，按当前用户安装。未签名包可能出现 SmartScreen 提示；确认来源和哈希后，若系统提供“更多信息 → 仍要运行”，可从该入口继续。包内为 WebView2 在线引导程序，缺少运行时时首次安装需联网，不能保证干净电脑离线首装。
- **升级**：先退出 App，备份下表中的数据目录；Mac 覆盖旧 App，Windows 运行新安装器。当前验证的是同一 0.1.0 包重装，不代表已验证跨版本迁移。
- **卸载**：Mac 将 App 移到废纸篓；Windows 从系统“已安装的应用”卸载。默认保留本地数据。Windows 卸载界面的“删除应用数据”选项尚未验证能清除自定义滤镜目录；需要彻底清除时，先备份，再手动删除下表目录。

| 平台 | 默认数据目录 |
| --- | --- |
| macOS | `~/Library/Application Support/DXO-ONE-XLAB/` |
| Windows | `%LOCALAPPDATA%\DXO-ONE-XLAB\` |

目录包含滤镜数据库、资源、预览照片副本、日志与 `card_backups/`；不要随安装包分发。电脑 App 安装与 App 内“更新补丁”（写入相机卡）是两个流程。

## 前置工具

1. macOS 的 Xcode Command Line Tools（`xcode-select --install`），或可用的完整 Xcode。Windows 需 Visual Studio Build Tools 的 C++ 桌面开发组件、Windows SDK、MSVC Rust 工具链及 Git Bash。
2. Rust/rustup；本目录 `rust-toolchain.toml` 固定 **1.99.1**，首次进入构建会获取该工具链；Mac 安装包脚本的工具链选择见下文。`Cargo.lock` 记录第三方 Rust 依赖版本，需随源码保留。
3. Python 3（构建脚本的路径处理）、Node.js **22**（UI 测试及通过 npx 调用打包 CLI；App 运行不需要 Node）。
4. SQLite 已启用 `bundled`，两端从源码编译，不需另装系统 SQLite 开发库。

当前前端直接嵌入 `dxo-app/ui/`，没有前端 npm 安装步骤，也不需要全局安装 `cargo tauri`；安装包脚本通过 npx 获取固定版本 Tauri CLI（默认 2.12.6）。
首次构建需要网络获取工具链和依赖；构建后的 App 资源自包含，不依赖源码目录。Windows 运行另需 WebView2。

## 从全新 clone 复现（macOS）

```bash
git clone https://github.com/Lohaslee/dxo-one-xlab.git
cd dxo-one-xlab/Rust重构/dxo-lut-tool
bash dist/verify-source.sh
open dxo-app/AppBundle/DXO-ONE-XLAB.app
```

`verify-source.sh` 顺序执行：Lightning 整包 19 件 SHA 校验 → UI 状态回归 → Rust 回归 → release App 构建与签名校验。
不运行 `#[ignore]` 的硬件/人工 golden 测试，不向真实相机卡写文件。

只需重新构建 App 时：

```bash
bash dist/build-app.sh
```

脚本可从任意工作目录调用；产物固定为 `dxo-app/AppBundle/DXO-ONE-XLAB.app`，不会创建第二份备份 App。
重复构建前请先正常退出正在运行的同一路径 App。运行时使用 macOS 系统 WebKit，不安装独立浏览器。

开发调试可使用 `cargo run --locked -p dxo-app`；当前正式复现入口仍是上述 release 脚本。
不同机器的 SDK、链接器与签名会影响二进制字节；这里验证的是同源码可构建并恢复相同功能，不承诺二进制逐字节相同。

## 构建安装包

保留完整仓库结构，在本目录分别于原生系统运行；不要在 Mac 上交叉打包后将其当作 Windows 安装验收。

macOS Apple Silicon：

```bash
node --test dxo-app/tests/ui-state.test.cjs
cargo test --locked -p lut-core -p lut-persist -p lut-camera -p dxo-app
bash dist/build-macos-release.sh
```

Windows x64（Git Bash，确保 MSVC 构建工具可用）：

```bash
node --test dxo-app/tests/ui-state.test.cjs
cargo test --locked -p lut-core -p lut-persist -p lut-camera -p dxo-app
bash dist/build-windows-release.sh
```

产物位于 `dist/release/`，各自附带 SHA-256 清单；两平台的文件汇总到同一交付目录后应重新生成完整清单。回归测试与 `build-app.sh` 使用 `--locked`；安装包脚本经 Tauri CLI 构建，未显式传入该选项。SDK、工具链及签名环境也会影响二进制字节。Mac 安装包脚本默认选择 `stable` 工具链，可用 `RUSTUP_TOOLCHAIN` 显式指定版本。

也可手动运行仓库的 [installers workflow](https://github.com/Lohaslee/dxo-one-xlab/actions/workflows/installers.yml)，两端分别在原生 runner 构建并上传附件，不自动创建公开 Release。未配置签名凭据时输出测试包；正式分发需另行完成 Developer ID 公证及 Windows Authenticode 签名。

2026-11-19 的 `13b8c82` 构建两端回归均通过，每端 23 项 UI 测试；macOS Rust 152 项通过、4 项忽略。Mac 本地安装复测通过，同包覆盖前后生成的数据文件哈希一致；Windows CI 已验证静默安装、启动、同包覆盖后的数据库哈希、卸载后数据库仍在，以及中文/空格安装路径。测试使用隔离数据目录，未预置完整的用户滤镜、照片和备份；旧版迁移、默认数据路径下卸载、干净 Windows 首装、真卡写入仍需补验。

## 不能遗漏的构建资源

保留仓库目录层级，不要单独拷贝 `dxo-app/`：

```text
仓库根/
├── DEPENDENCIES.md                       # 编译时嵌入触屏来源说明
├── Lightning-L口/                       # 已跟踪的 Build 72 完整 19 件 + SHA256SUMS
└── Rust重构/dxo-lut-tool/
    ├── Cargo.toml / Cargo.lock / rust-toolchain.toml
    ├── dxo-app/                          # App、UI、字体/许可证、图标、合成图像测试夹具
    ├── lut-core/resources/Imx183_Cc_Still1.bin
    ├── lut-core/ / lut-persist/ / lut-camera/
    ├── dxo-cli/ / dxo-gui/               # workspace 内的旧入口，保留源码供解析/研究
    ├── tests/fixtures/                   # 3DL / CSP / Hald 合成基线，已生成可直接测试
    └── dist/                            # 当前 App 构建与验证脚本
```

App 的 `build.rs` 从仓库根 `Lightning-L口/SHA256SUMS.txt` 生成内嵌文件清单，启动/预检时验证内容 SHA。
`lut-core/resources` 的原厂 CC 表与已发布的 `自定义置入滤镜流程/stock/` 同字节；第三方权利说明见根目录 LICENSE / THIRD_PARTY.md。
测试夹具已入仓，正常测试不需要本地 `工具/filt_core.py`、`golden/` 或 `实验/`。

## XMP 预设导入

在「导入滤镜」中选择或拖入 `.xmp`，与其他 LUT 一样入库、预览、调整强度并加入卡清单。滤镜库持续标注 **XMP 近似**；导入结果以警告显示未转换的非零/启用参数。单个 XMP 上限 4MB，格式错误逐文件报告且不入库。

- 支持 Lightroom / Camera Raw 普通预设中的 `Exposure2012`、`Contrast2012`、`Highlights2012`、`Shadows2012`、`Whites2012`、`Blacks2012`、饱和度、自然饱和度、八色 HSL，以及主曲线/RGB 通道点曲线（PV2012 或旧点曲线）。
- 采用显示 RGB 上的近似算法，烘焙为 16³ LUT：曝光倍率 → 明暗/对比度 → 主曲线/通道曲线（分段线性）→ HSL/饱和度。它不实现 Adobe RAW 解码、Process Version 或 Adobe 色彩引擎，**不保证与 Lightroom / Camera Raw 原效果一致**。
- 白平衡、相机配置、校准、清晰度/纹理/去朦胧、参数曲线、色彩分级、黑白混合、锐化/降噪、镜头校正、蒙版等暂不转换；启用参数会列在导入日志中。包含 Adobe Look/Profile 内嵌数据的 XMP 整份拒绝，避免漏掉其核心效果。只有不支持的参数时也会拒绝。
- 转换后的源 LUT 与 CC 表都持久保存，重启后仍可预览和重算强度；预览与写卡复用同一 CC 表。此一致性不表示复现了 Adobe 的原始处理。

## 运行数据与旧版接口

当前 Tauri App 在界面扫描并选择存储卡，**不绑定开发者的卡 UUID**。
本地数据目录见上方[安装与升级](#安装与升级)，不随仓库或 App 打包。
需要隔离试用时，直接启动可执行文件并设置独立目录：

```bash
DXO_DATA_DIR="$(mktemp -d)" dxo-app/AppBundle/DXO-ONE-XLAB.app/Contents/MacOS/dxo-app
```

旧 `dxo-cli` / Iced GUI 的 `find_card` 接口需显式设置 `DXO_CARD_UUID`，未设置时报告未配置，避免猜测写入对象。
该配置不影响当前 Tauri App 的正常扫描选卡；不应把自己的 UUID 写回源码。

## 现有功能与保护

- 滤镜导入、参数保存、同源表预览、卡清单、相机名称提示、选择不闪动。
- 自动记住最后一张预览照片：仅保存已转正、最长边不超过 1024 的无损副本，更换时替换，重启自动恢复；「重置色卡」清除副本。
- 内置 Lightning Build 72 完整包，适配固件 3.2.0 / `ecef5ef809`；L口改C口仍按机身固件判断。
- 纯 C 口入口显示“开发测试中，敬请期待”；不能混装 Lightning 补丁。
- 写卡先只读预检、展示目标、一次确认；共享写卡锁，未保存参数/换卡/内容变化会阻断。
- 先本地备份，逐文件暂存、刷盘、替换、回读；失败恢复并报告备份位置，不承诺断电/拔卡时整批原子性。
- 保留照片、已有触屏偏好和未知文件；补丁安装与滤镜编辑使用各自白名单。

真卡安装必须在 App 内人工确认。源码复现测试使用临时目录，**不代表已经验证过全新 App 的真卡安装和相机开机**。
本次源码复现证据及剩余验证范围见 [SOURCE-REPRODUCIBILITY.md](docs/SOURCE-REPRODUCIBILITY.md)。
最初的跨平台交付要求见 [AI 执行交接](../../AI执行交接-Mac与Windows安装包.md)；当前支持范围与构建入口以本 README 为准。

## 单独运行测试

```bash
node --test dxo-app/tests/ui-state.test.cjs
cargo test --locked -p lut-core -p lut-persist -p lut-camera -p dxo-app
```

4 项忽略测试涉及真实卡或人工提供的 Python 对照产物，默认不运行；不要为了消除 ignored 数量直接启用。
历史 Iced GUI/CLI 不作为当前 App 验收入口，当前不发布旧 `build-release.sh` / `deploy.sh`。
