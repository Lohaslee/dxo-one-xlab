# DXO-ONE-XLAB 桌面源码 · 0.1.0

当前 App 使用 **Tauri 2 + Rust + 静态 HTML**。本目录包含完整构建输入和测试；无需原开发机目录、用户数据库、原厂固件镜像或私有工具脚本。

## 支持范围

- 当前验证平台：macOS 26.1 / Apple Silicon（arm64）。Intel Mac 和更早 macOS 尚未验证。
- Windows / Linux 不是本次支持平台；读卡与补丁安装仍依赖 macOS `diskutil`、`/Volumes` 和 Unix 同步 API。
- 输出为本地可运行 `.app`，使用 ad-hoc 签名。没有正式 Developer ID 签名、公证或 DMG/EXE 安装程序。
- 软件预览、保存及写卡复用同源 CC 表；不等同于已经完成相机实拍最终色彩一致性验证。

## 前置工具

1. macOS 的 Xcode Command Line Tools（`xcode-select --install`），或可用的完整 Xcode。需提供 clang、macOS SDK、`codesign` 等工具。
2. Rust/rustup；本目录 `rust-toolchain.toml` 固定 **1.99.1**，首次进入构建会获取该工具链。`Cargo.lock` 固定第三方 Rust 依赖，所有发布构建使用 `--locked`。
3. Python 3（构建脚本的路径处理）、Node.js **22**（仅 UI 测试；App 运行不需要 Node）。
4. 系统 SQLite 3 开发接口（当前 rusqlite 不启用 bundled；已在 macOS SDK/系统库验证）。

当前前端直接嵌入 `dxo-app/ui/`，没有 npm 安装步骤，也不需要额外安装 `cargo tauri`。
首次构建需要网络获取工具链和依赖；构建后的 App 资源自包含，不依赖网络或源码目录。

## 从全新 clone 复现

```bash
git clone https://github.com/Lohaslee/dxo-one-xlab.git
cd dxo-one-xlab/Rust重构/dxo-lut-tool
bash dist/verify-source.sh
open dxo-app/AppBundle/DXO-ONE-XLAB.app
```

`verify-source.sh` 顺序执行：Lightning 整包 19 件 SHA 校验 → 19 项 UI 状态回归 → Rust 回归 → release App 构建与签名校验。
不运行 `#[ignore]` 的硬件/人工 golden 测试，不向真实相机卡写文件。

只需重新构建 App 时：

```bash
bash dist/build-app.sh
```

脚本可从任意工作目录调用；产物固定为 `dxo-app/AppBundle/DXO-ONE-XLAB.app`，不会创建第二份备份 App。
重复构建前请先正常退出正在运行的同一路径 App。运行时使用 macOS 系统 WebKit，不安装独立浏览器。

开发调试可使用 `cargo run --locked -p dxo-app`；当前正式复现入口仍是上述 release 脚本。
不同机器的 SDK、链接器与签名会影响二进制字节；这里验证的是同源码可构建并恢复相同功能，不承诺二进制逐字节相同。

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

## 运行数据与旧版接口

当前 Tauri App 在界面扫描并选择存储卡，**不绑定开发者的卡 UUID**。
本地数据默认保存在 `~/Library/Application Support/DXO-ONE-XLAB/`，包括数据库、资源、日志和 `card_backups/`；不随仓库或 App 打包。
需要隔离试用时，直接启动可执行文件并设置独立目录：

```bash
DXO_DATA_DIR="$(mktemp -d)" dxo-app/AppBundle/DXO-ONE-XLAB.app/Contents/MacOS/dxo-app
```

旧 `dxo-cli` / Iced GUI 的 `find_card` 接口需显式设置 `DXO_CARD_UUID`，未设置时报告未配置，避免猜测写入对象。
该配置不影响当前 Tauri App 的正常扫描选卡；不应把自己的 UUID 写回源码。

## 现有功能与保护

- 滤镜导入、参数保存、同源表预览、卡清单、相机名称提示、选择不闪动。
- 内置 Lightning Build 72 完整包，适配固件 3.2.0 / `ecef5ef809`；L口改C口仍按机身固件判断。
- 纯 C 口入口显示“开发测试中，敬请期待”；不能混装 Lightning 补丁。
- 写卡先只读预检、展示目标、一次确认；共享写卡锁，未保存参数/换卡/内容变化会阻断。
- 先本地备份，逐文件暂存、刷盘、替换、回读；失败恢复并报告备份位置，不承诺断电/拔卡时整批原子性。
- 保留照片、已有触屏偏好和未知文件；补丁安装与滤镜编辑使用各自白名单。

真卡安装必须在 App 内人工确认。源码复现测试使用临时目录，**不代表已经验证过全新 App 的真卡安装和相机开机**。
本次源码复现证据及剩余验证范围见 [SOURCE-REPRODUCIBILITY.md](docs/SOURCE-REPRODUCIBILITY.md)。
后续跨平台安装包任务见 [AI 执行交接](../../AI执行交接-Mac与Windows安装包.md)。

## 单独运行测试

```bash
node --test dxo-app/tests/ui-state.test.cjs
cargo test --locked -p lut-core -p lut-persist -p lut-camera -p dxo-app
```

4 项忽略测试涉及真实卡或人工提供的 Python 对照产物，默认不运行；不要为了消除 ignored 数量直接启用。
历史 Iced GUI/CLI 不作为当前 App 验收入口，当前不发布旧 `build-release.sh` / `deploy.sh`。
