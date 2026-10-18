<p align="center">
  <img src="./assets/branding/icon.png" width="128" alt="Pilo 应用图标">
</p>

<h1 align="center">Pilo</h1>

<p align="center">简洁的 Pi Coding Agent 桌面客户端：在一个工作区中管理 Local、WSL 与 SSH 项目，同时让 Pi 始终运行在代码所在的环境。</p>

<p align="center">
  <a href="./README.md">English</a> | <a href="./README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <a href="https://github.com/gaoxiang89/pilo/actions/workflows/ci.yml"><img src="https://github.com/gaoxiang89/pilo/actions/workflows/ci.yml/badge.svg" alt="CI 状态"></a>
  <img src="https://img.shields.io/badge/Desktop-Windows%20x64%20%7C%20macOS%20arm64-4B5563?style=flat-square" alt="桌面发行平台：Windows x64 与 macOS arm64">
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Rust-2024-3776AB?style=flat-square" alt="Rust 2024 edition"></a>
  <a href="https://tauri.app/"><img src="https://img.shields.io/badge/Tauri-2-3776AB?style=flat-square" alt="Tauri 2"></a>
  <a href="https://react.dev/"><img src="https://img.shields.io/badge/React-19-3776AB?style=flat-square" alt="React 19"></a>
</p>

Pilo 是 [Pi Coding Agent](https://pi.dev/) 的桌面客户端，不重新实现 Agent。它负责在统一工作区中呈现 Connection、Project 与 Pi Session，而会话状态和 Agent 行为仍由 Pi 负责。

> 本仓库 fork 自 [hsingjui/pilo](https://github.com/hsingjui/pilo)。感谢原作者 [@hsingjui](https://github.com/hsingjui) 创建了 Pilo。

## 核心能力

| 能力                               | 为什么重要                                                                                            |
| ---------------------------------- | ----------------------------------------------------------------------------------------------------- |
| Local、WSL、SSH 统一工作区         | 项目保留在原本的环境中；Pilo 会在所选环境中运行 `pilo-server`，不需要先把远程项目复制到桌面端。       |
| Pi 运行在代码所在环境              | 服务端以项目目录为工作目录启动 `pi --mode rpc`，Local、WSL 与 SSH 使用一致的交互方式。                |
| Pi 保持事实来源                    | Session 直接读取 Pi 的 JSONL 文件；SQLite 只保存可重建的索引、缓存和桌面状态。                        |
| Chat、工具、终端、文件与 Diff 同窗 | Pilo 展示 Pi 的思考与工具调用，并整合 PTY 终端、文件操作、预览端口以及 Git 状态/Diff。                |
| 并行 Agent                         | 同一项目可以运行多个 Agent 流，并分别列出、发送消息或停止。                                           |
| 原生桌面更新                       | Release 构建会为 Windows x64 与 macOS arm64 生成 updater 产物，Pilo 可以从 GitHub Releases 检查更新。 |

## 架构

Pilo 由桌面应用和运行在目标 Connection 环境中的 `pilo-server` 组成。Local 直接运行，WSL 通过 `wsl.exe`，SSH 则向远端部署匹配平台的服务端二进制。桌面端与服务端通过 `crates/pilo-protocol` 定义的 protobuf `Envelope` 帧在 stdio 上通信。

```text
┌──────────────────────────────┐
│        Pilo Desktop          │
│       React + Tauri 2        │
└──────────────┬───────────────┘
               │ pilo-protocol / stdio
               ▼
┌──────────────────────────────┐
│          pilo-server         │
│   Local | WSL | SSH target   │
└──────────────┬───────────────┘
               │ pi --mode rpc
               ▼
┌──────────────────────────────┐
│       Pi Coding Agent        │
└──────────────────────────────┘
```

## 快速安装

Pilo 当前以源码构建为主。开发需要 Node.js 22、pnpm（CI 使用 10.12.3）、Rust stable 工具链，以及在每个准备使用的环境中安装 Pi。

Pilo 默认从 `PATH` 自动发现 `pi`。也可以在 Settings 中为每个 Connection 单独指定 Pi 可执行文件路径。

## Factory Droid

新建会话可以选择 [Factory Droid](https://docs.factory.ai/) 而不是 Pi。连接记住默认代理，会话保持创建时的选择。

Droid 跟 Pi 走同一套 Local、WSL、SSH 服务，跑在项目所在的环境里。需要先在那个环境安装并登录 `droid`（或设置 `FACTORY_API_KEY`）。Pilo 不保存这把密钥。会话历史读取 `~/.factory/sessions`，SQLite 只做索引。

这一版只做对话闭环：回复和工具调用，使用 Droid 自己的默认模型，自主级别固定为 medium。Pi 扩展、斜杠命令、模型选择和权限确认仍只属于 Pi 会话。

### Windows

```powershell
pnpm install
pnpm dev
```

### macOS / Linux

```bash
pnpm install
pnpm dev
```

`pnpm dev`（或 `pnpm tauri dev`）启动时不再构建 WSL/SSH 运行时。桌面前端热更新、WebUI 资源监听和 Tauri 原有的 Rust 自动重编译保持不变，不再增加 server/插件构建监听。

Local 使用桌面构建中内嵌的 server。WSL/SSH 请先手动构建开发 server，再启动或重启 Pilo：

```powershell
pnpm server:build
pnpm dev
```

Windows 请在 Windows 那份仓库中执行以上命令。手动构建通过 WSL 运行，WSL 中需安装 Rust 和 Bash，默认构建该发行版原生架构的 Linux server。可在构建前指定发行版，例如 `$env:PILO_DEV_WSL_DISTRO = "Debian"`。macOS/Linux 默认构建原生 server。其他远程目标可使用 `pnpm server:build linux-aarch64`，构建环境中需安装对应 Rust target 和链接器。

dev 构建默认使用一个 Cargo job，并关闭调试符号，降低 WSL 内存占用；需要时可显式覆盖 `CARGO_BUILD_JOBS` / `CARGO_PROFILE_DEV_DEBUG`。正式构建参数不变。

开发二进制直接使用 Cargo 标准输出目录，例如 `target/x86_64-unknown-linux-gnu/debug/pilo-server`，不再额外复制一份；`src-tauri/resources/` 中的正式资源保持不变。启动或连接时，Pilo 将匹配的 server 部署到 dev 运行目录，server 自动将内嵌插件释放到 `extensions/`。修改插件后重新构建并重启 Pilo/Pi 即可加载。运行时和插件缓存使用 `pilo-dev/server`，而非正式版的 `pilo/server`（WSL/SSH 为 `~/.cache/pilo-dev/server`）；Pi 设置和会话文件仍然共享。

`scripts/sync-to-windows.sh` 只同步源码，不同步 server 构建产物。同步修改后，请在 Windows 那份仓库中执行手动 dev 构建。

Release 打包流程仍自动准备完整运行时集合。手动准备正式资源时，Windows 使用 `pnpm server:build:win`，macOS/Linux 使用 `bash scripts/build-pilo-server.sh`。

支持的服务端运行时目标为 Linux x64/arm64、Windows x64/arm64 和 macOS x64/arm64。也可以显式构建指定目标：

```bash
bash scripts/build-pilo-server.sh <target>
```

## 快速开始

1. 使用 `pnpm tauri dev` 启动 Pilo。
2. 使用默认的 Local Connection，或添加 WSL / SSH Connection。
3. 如果 `pi` 不在 `PATH` 中，在 Settings 中为对应 Connection 指定 Pi 可执行文件路径并执行探测。
4. 添加项目目录并打开 Session。
5. 发送消息。Pilo 会在项目目录中启动 `pi --mode rpc`，并把会话流式呈现在工作区中。

对于 WSL 或 SSH，Pilo 会先探测目标平台并部署匹配的 `pilo-server` 运行时。如果对应运行时尚未准备，Pilo 会明确报告缺失的资源，而不是使用不兼容的二进制。

## 自定义主题

在 **设置 → 外观** 中选择 **Lody**、**Claude** 或已保存的 CSS 主题。
创建主题时，在 **自定义 CSS** 中填写名称和 CSS，点击 **保存并应用**。
已连接的 WebUI 会跟随桌面 Host 的主题。

入门示例、明暗模式覆盖、可用令牌及恢复方法见[主题编写指南](docs/THEMES.md)（英文）。

## 回复中的图片

让 Pi 将截图保存在项目目录内，并在回复中使用普通 Markdown 引用：

```markdown
这是当前页面：

![页面截图](./screenshots/page.png)

右上角的按钮存在对齐问题。
```

桌面端和 WebUI 会从项目所在的 Local、WSL 或 SSH 环境加载相对路径图片。支持 PNG、JPEG、WebP、GIF，单张不超过 10 MB；点击图片可放大预览或保存。不支持绝对路径、父目录跳转和 SVG，普通 HTTP(S) 图片也能嵌入正文，预览中提供“打开原图”的新标签页入口，不直接下载。无需 Pi 插件或修改 Pi 核心。

图片引用现有文件，不是不可变快照：删除或覆盖文件可能影响旧回复。这项功能不会增加截图工具，也不会自动修改 Pi 的提示词。

## 开发

| 命令                                                                   | 用途                                          |
| ---------------------------------------------------------------------- | --------------------------------------------- |
| `pnpm check`                                                           | 运行 oxfmt 格式检查和 oxlint                  |
| `pnpm format`                                                          | 使用 oxfmt 格式化前端代码                     |
| `pnpm build`                                                           | TypeScript 类型检查并构建前端                 |
| `pnpm test`                                                            | 运行 Node 单元测试和集成测试                  |
| `pnpm test:unit`                                                       | 运行单元测试和静态渲染测试                    |
| `pnpm test:integration`                                                | 运行 CLI/进程及 Pi 扩展集成测试（Linux/WSL）  |
| `pnpm test:packaging`                                                  | 运行 AppImage 打包编排测试（Linux/WSL）       |
| `cargo fmt --all --check`                                              | Rust 格式检查                                 |
| `cargo check --workspace --all-targets --all-features`                 | Rust workspace 检查                           |
| `cargo test --workspace --all-features`                                | Rust workspace 测试                           |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Rust lint                                     |
| `pnpm server:verify`                                                   | 校验发布所需的完整六平台 `pilo-server` 资源集 |

测试目录、单文件运行命令及浏览器回归页面见 [tests/README.md](tests/README.md)。
`pnpm test` 不包含浏览器、打包或 Rust 测试。

### 桌面 WebDriver 调试

先用 `pnpm dev` 启动桌面开发版，再在同一台机器的另一个终端执行：

```bash
pnpm debug:desktop                   # 检查内嵌 WebDriver 状态
pnpm debug:desktop windows           # 列出窗口标签
pnpm debug:desktop screenshot        # 保存 PNG 到 logs/（已忽略提交）
pnpm debug:desktop source            # 输出当前 DOM
pnpm debug:desktop eval 'return document.activeElement.outerHTML'
pnpm debug:desktop click 'button'
pnpm debug:desktop type '[contenteditable="true"]' 'Hello'
pnpm debug:desktop --window session-123 source
```

脚本零依赖，直接连接已有的内嵌 WebDriver，默认地址为 `127.0.0.1:4445`。
应用使用自定义端口时，通过 `--port` 或 `TAURI_WEBDRIVER_PORT` 指定相同端口。
默认操作 `main` 窗口，完整命令见 `--help`。每次执行后释放自动化会话，不关闭 Pilo。
点击、输入和脚本会操作真实应用状态；输入命令追加文字，不清空原内容。
此脚本调试桌面 WebView，不替代 WebUI 浏览器测试；正式 release 构建不启用 WebDriver。
若通过 `pnpm tauri dev` 启动，需显式加上 `--features webdriver`，
而 `pnpm dev` 已默认启用。

## 目录结构

| 路径                    | 内容                                                                |
| ----------------------- | ------------------------------------------------------------------- |
| `src/`                  | React UI：聊天、Session、侧边栏、设置、终端与项目交互               |
| `src-tauri/`            | Tauri 应用：命令、运行时管理、Connection/Project 状态与 SQLite 索引 |
| `crates/pilo-protocol/` | 桌面应用与 `pilo-server` 共用的通信协议                             |
| `crates/pilo-server/`   | 运行在目标环境中的 Pi 流、终端、文件系统、预览端口与 Session 索引   |
| `scripts/`              | 运行时构建与资源校验脚本                                            |
| `docs/`                 | 设计文档                                                            |

## 社区

[linux.do](https://linux.do)

## 第三方开源声明

Pilo 的部分代码派生或改编自第三方开源软件，包括 Lody。来源与许可证详情见 [THIRD_PARTY_NOTICES.md](./THIRD_PARTY_NOTICES.md)。

## 开源许可证

Pilo 使用 [Apache License 2.0](./LICENSE) 开源。

第三方代码来源与署名信息见 [THIRD_PARTY_NOTICES.md](./THIRD_PARTY_NOTICES.md)。
