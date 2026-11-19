# 源码复现验收记录

验收日期：2026-11-19。结论：仅使用准备提交的 Git 文件，在空构建目录生成 macOS arm64 App；将产物移出源码目录后，使用全新数据目录成功启动并显示主界面。

## 受测输入与环境

- 受测 Git tree：`6ecd0bfcbbfb8775559d55b052278fa6b3f7cadd`。这是补入本报告及根目录文档修正之前的暂存快照；生产源码、锁文件和构建资源与本次提交一致。
- macOS 26.1 / Apple Silicon arm64；Xcode 26.2；Node.js 22.22.1；Python 3.9.6。
- rustc / cargo 1.99.1；rustc commit `58cf8f9c3`（2026-10-13）。
- `Cargo.lock` SHA-256：`ca51a63da336f2e9aceb2d69cb3d0cf2450b39839180ab3c28e2112187ea7948`。
- `dxo-app/ui/index.html` SHA-256：`9aca62e65b1686d303d0af78ae55e1d414b8baa351846aeb4c727274efd33c3a`。

仓库通过 `rust-toolchain.toml` 固定 1.99.1。本机精确命名工具链的安装出现中断，因此本次使用已安装的 `stable`，并在执行前断言其版本和 rustc commit 与 1.99.1 完全相同。实际命令为 `RUSTUP_TOOLCHAIN=stable bash dist/verify-source.sh`。这次验证未覆盖 rustup 首次下载固定工具链的过程；后续执行者按 README 使用固定工具链即可，不应长期改为浮动 stable。

## 方法与结果

从暂存区生成 Git tree 并用 `git archive` 导出到临时目录。导出内容不包含开发机的 `target/`、数据库、实验目录、外部工具或原有 AppBundle；首次构建仍使用本机的工具链及 Cargo 下载缓存。

在导出的 `Rust重构/dxo-lut-tool/` 执行 `dist/verify-source.sh`，整个脚本退出码为 0：

| 检查 | 结果 |
| --- | --- |
| Lightning Build 72 完整包 SHA-256 | 19 件全部匹配 |
| Node UI 状态回归 | 19 通过，0 失败 |
| Rust 测试及文档测试 | 132 通过，0 失败，4 忽略 |
| `cargo build --locked --release -p dxo-app` | 成功，生成 arm64 可执行文件 |
| `.app` 组装与 ad-hoc 签名 | 成功；`codesign --verify --deep --strict` 通过 |
| 构建脚本的个人路径扫描 | 通过 |

4 项 ignored 测试需要真实存储卡或人工准备的对照产物，未强行启用。编译存在已有的未使用代码警告，没有编译错误。

随后把 `.app` 移到源码树外，将原编译源码目录改名，使编译时路径不存在；直接启动 `.app/Contents/MacOS/dxo-app`，指定全新的 `DXO_DATA_DIR`。进程持续运行，并建立数据库、store、cubes 与日志目录。通过 macOS 应用界面核对到完整主界面：菜单、空滤镜库、预览服务就绪和工作台启动日志均正常显示。试运行数据与用户原有数据隔离，验收后删除临时产物。

## 验证范围

本记录验证从 Git 源码构建及独立启动能力；不同 SDK、链接器和签名可能改变产物字节，不承诺二进制逐字节相同。

尚未在 Intel Mac、更早 macOS、Windows 或 Linux 上验收；尚未验证 Developer ID 签名、公证、DMG/EXE 安装程序。本轮没有向真实卡写入补丁，也没有进行相机开机或实拍色彩验收。

复现命令见 [README](../README.md)，后续安装包工作见 [AI 执行交接](../../../AI执行交接-Mac与Windows安装包.md)。本机详细日志留存在被忽略的 `docs/verify/source-release-20261119/`，不作为其他机器构建的必需输入。
