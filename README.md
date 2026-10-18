> [!WARNING]
> **个人使用的非官方 fork / Unofficial fork for personal use**
>
> 这是 [@gaoxiang89](https://github.com/gaoxiang89) 个人使用的修改版，不面向其他用户。使用 Pilo 请前往官方原版：**[hsingjui/pilo](https://github.com/hsingjui/pilo)**。
>
> This is a modified build for [@gaoxiang89](https://github.com/gaoxiang89)'s personal use only and is not intended for other users. To use Pilo, please use the official version: **[hsingjui/pilo](https://github.com/hsingjui/pilo)**.
>
> 感谢原作者 [@hsingjui](https://github.com/hsingjui) 创建了 Pilo。Thanks to the original author [@hsingjui](https://github.com/hsingjui) for creating Pilo.
>
> 本 fork 相对原版的修改 / Modifications in this fork (Apache License 2.0)：
>
> - 新增 Factory Droid 运行时 / Added a Factory Droid runtime
> - 自动升级源和签名密钥改为本仓库 / Updater source and signing key point to this repository
> - 增加本说明 / Added this notice

<p align="center">
  <img src="./assets/branding/icon.png" width="128" alt="Pilo application icon">
</p>

<h1 align="center">Pilo</h1>

<p align="center">A simple desktop client for Pi Coding Agent that brings Local, WSL, and SSH projects into one workspace while keeping Pi in the environment where the code lives.</p>

<p align="center">
  <a href="./README.md">English</a> | <a href="./README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <a href="https://github.com/gaoxiang89/pilo/actions/workflows/ci.yml"><img src="https://github.com/gaoxiang89/pilo/actions/workflows/ci.yml/badge.svg" alt="CI status"></a>
  <img src="https://img.shields.io/badge/Desktop-Windows%20x64%20%7C%20macOS%20arm64-4B5563?style=flat-square" alt="Desktop releases: Windows x64 and macOS arm64">
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Rust-2024-3776AB?style=flat-square" alt="Rust 2024 edition"></a>
  <a href="https://tauri.app/"><img src="https://img.shields.io/badge/Tauri-2-3776AB?style=flat-square" alt="Tauri 2"></a>
  <a href="https://react.dev/"><img src="https://img.shields.io/badge/React-19-3776AB?style=flat-square" alt="React 19"></a>
</p>

Pilo is a desktop client for [Pi Coding Agent](https://pi.dev/), not a separate agent implementation. It presents connections, projects, and Pi sessions in an IDE-style workspace while leaving conversation state and agent behavior to Pi.

## Highlights

| Highlight                                         | Why it matters                                                                                                                                       |
| ------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| Local, WSL, and SSH in one workspace              | Projects stay in their original environments. Pilo runs `pilo-server` in the selected environment instead of copying remote projects to the desktop. |
| Pi runs where the project lives                   | The server starts `pi --mode rpc` with the project directory as its working directory for Local, WSL, and SSH connections.                           |
| Pi stays the source of truth                      | Sessions are read from Pi JSONL files; SQLite only stores rebuildable indexes, caches, and desktop state.                                            |
| Chat, tools, terminals, files, and diffs together | Pilo renders Pi thinking and tool calls alongside PTY terminals, file operations, preview ports, and Git status/diffs.                               |
| Parallel agents                                   | Multiple agent streams can run for a project and can be listed, messaged, or stopped independently.                                                  |
| Native desktop updates                            | Release builds produce updater artifacts for Windows x64 and macOS arm64, and Pilo can check for updates from GitHub Releases.                       |

## Architecture

Pilo consists of a desktop app and a `pilo-server` process that runs in the selected connection environment. Local runs directly, WSL uses `wsl.exe`, and SSH deploys a matching server binary to the remote host. Desktop/server communication uses protobuf `Envelope` frames over stdio through `crates/pilo-protocol`.

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

## Quick Install

Pilo is currently built from source. Development requires Node.js 22, pnpm (CI uses 10.12.3), a stable Rust toolchain, and Pi installed in every environment you plan to use.

Pilo discovers `pi` from `PATH` by default. Each connection can also be configured with a custom Pi executable path in Settings.

## Factory Droid

A new session can run [Factory Droid](https://docs.factory.ai/) instead of Pi. The connection remembers the default; the session keeps the agent it was created with.

Droid runs through the same Local, WSL, and SSH server as Pi, in the environment where the project lives. Install and log in to `droid` there (or set `FACTORY_API_KEY` in that environment). Pilo does not store the key. Session history is read from `~/.factory/sessions`; SQLite only indexes those files.

This first version is a conversation loop: replies and tool calls, with Droid's own default model and medium autonomy. Pi extensions, slash commands, model selection, and permission prompts stay on Pi sessions.

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

`pnpm dev` (or `pnpm tauri dev`) starts development without building WSL/SSH runtimes. Desktop frontend HMR, WebUI asset watching and Tauri's normal Rust rebuild behavior remain enabled; there is no additional server/extension build watcher.

Local uses the server embedded in the desktop build. For WSL/SSH, build a development server manually, then start or restart Pilo:

```powershell
pnpm server:build
pnpm dev
```

On Windows, run these commands in the Windows checkout. The manual build uses WSL (Rust and Bash must be installed there), with its native Linux target by default. Set `PILO_DEV_WSL_DISTRO` before building to select a distro, for example `$env:PILO_DEV_WSL_DISTRO = "Debian"`. On macOS/Linux, it builds the native server target. To build another remote target, use `pnpm server:build linux-aarch64`; its Rust target and linker must be installed in the build environment.

The dev build defaults to one Cargo job and no debug symbols to reduce WSL memory use. Override `CARGO_BUILD_JOBS` / `CARGO_PROFILE_DEV_DEBUG` explicitly if needed. Production build settings are unchanged.

Development binaries stay in Cargo’s standard output directory, for example `target/x86_64-unknown-linux-gnu/debug/pilo-server`, without a second staging copy. Release resources in `src-tauri/resources/` remain unchanged. On startup/connection, Pilo deploys the matching server into its dev runtime directory; the server writes its embedded plugins into `extensions/`. Rebuild after plugin changes and restart Pilo/Pi to load them. Runtime/extension caches use `pilo-dev/server` instead of `pilo/server` (WSL/SSH: `~/.cache/pilo-dev/server`). Pi settings and session files remain shared.

`scripts/sync-to-windows.sh` syncs source only, not server build artifacts. Run the manual dev build in the Windows checkout after syncing changes.

Release packaging stages the complete runtime set automatically. To stage a release runtime manually, use `pnpm server:build:win` on Windows or `bash scripts/build-pilo-server.sh` on macOS/Linux.

Supported server runtime targets are Linux x64/arm64, Windows x64/arm64, and macOS x64/arm64. An explicit runtime can be built with:

```bash
bash scripts/build-pilo-server.sh <target>
```

## Quick Start

1. Start Pilo with `pnpm tauri dev`.
2. Use the default Local connection, or add a WSL or SSH connection.
3. If Pi is not available as `pi` on `PATH`, set its executable path for that connection in Settings and run the Pi probe.
4. Add a project directory and open a session.
5. Send a message. Pilo starts `pi --mode rpc` in the project directory and streams the session into the workspace.

For WSL or SSH, Pilo probes the target platform and deploys the matching `pilo-server` runtime before starting it. If that runtime has not been staged, Pilo reports the missing resource instead of silently using an incompatible binary.

## Custom themes

Choose **Lody**, **Claude**, or a saved CSS theme in **Settings → Appearance**.
To create a theme, enter a name and CSS in **Custom CSS**, then choose
**Save and apply**. Connected WebUI clients follow the desktop Host theme.

See the [theme customization guide](docs/THEMES.md) for a starter theme,
light/dark overrides, supported tokens, and recovery tips.

## Images in replies

Ask Pi to save screenshots inside the project and include them in its reply using ordinary Markdown:

```markdown
Here is the current page:

![Page screenshot](./screenshots/page.png)

The button in the upper-right corner is misaligned.
```

Desktop and WebUI load project-relative images from the project's Local, WSL, or SSH environment. PNG, JPEG, WebP, and GIF are supported, up to 10 MB each; click an image to preview or save it. Absolute paths, parent-directory traversal, and SVG are not supported. HTTP(S) images also render inline and offer an Open original image action in a new tab instead of a direct download. No Pi plugin or core changes are required.

Images reference existing files, not immutable snapshots: deleting or overwriting a file can affect old replies. This feature does not add screenshot capture tools or automatically change Pi's prompts.

## Development

| Command                                                                | Purpose                                                           |
| ---------------------------------------------------------------------- | ----------------------------------------------------------------- |
| `pnpm check`                                                           | Run oxfmt format checks and oxlint                                |
| `pnpm format`                                                          | Format the frontend with oxfmt                                    |
| `pnpm build`                                                           | Type-check and build the frontend                                 |
| `pnpm test`                                                            | Run Node unit and integration tests                               |
| `pnpm test:unit`                                                       | Run focused unit and static-rendering tests                       |
| `pnpm test:integration`                                                | Run CLI/process and Pi extension integration tests (Linux/WSL)    |
| `pnpm test:packaging`                                                  | Run AppImage orchestration tests (Linux/WSL)                      |
| `cargo fmt --all --check`                                              | Check Rust formatting                                             |
| `cargo check --workspace --all-targets --all-features`                 | Check the Rust workspace                                          |
| `cargo test --workspace --all-features`                                | Run Rust workspace tests                                          |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Run Rust lints                                                    |
| `pnpm server:verify`                                                   | Verify the complete six-target `pilo-server` release resource set |

Test layout, single-file commands and browser regression pages are documented
in [tests/README.md](tests/README.md). `pnpm test` does not include browser,
packaging or Rust tests.

### Desktop WebDriver debugging

Start the desktop development build with `pnpm dev`, then use another terminal
on the same machine:

```bash
pnpm debug:desktop                   # Check the embedded WebDriver server
pnpm debug:desktop windows           # List window labels
pnpm debug:desktop screenshot        # Save a PNG under logs/ (gitignored)
pnpm debug:desktop source            # Print the current DOM
pnpm debug:desktop eval 'return document.activeElement.outerHTML'
pnpm debug:desktop click 'button'
pnpm debug:desktop type '[contenteditable="true"]' 'Hello'
pnpm debug:desktop --window session-123 source
```

The zero-dependency script connects directly to the existing embedded WebDriver
at `127.0.0.1:4445`. Use `--port` or `TAURI_WEBDRIVER_PORT` to match a custom
app port. The default window is `main`; `--help` lists all commands. Each command
releases its automation session without closing Pilo. Click, typing, and scripts
operate on real application state; typing appends rather than clearing the field.
This is desktop WebView debugging, not WebUI browser testing. Release builds do
not enable the WebDriver plugin. For `pnpm tauri dev`, explicitly add
`--features webdriver` (`pnpm dev` already does this).

## Repository layout

| Path                    | Contents                                                                                                        |
| ----------------------- | --------------------------------------------------------------------------------------------------------------- |
| `src/`                  | React UI: chat, sessions, sidebar, settings, terminals, and project interactions                                |
| `src-tauri/`            | Tauri app: commands, runtime managers, connection/project state, and SQLite indexes                             |
| `crates/pilo-protocol/` | Wire protocol shared by the desktop app and `pilo-server`                                                       |
| `crates/pilo-server/`   | Pi streams, terminals, filesystem operations, preview ports, and session indexing inside the target environment |
| `scripts/`              | Runtime build and verification scripts                                                                          |
| `docs/`                 | Design notes                                                                                                    |

## Community

[linux.do](https://linux.do)

## Third-Party Notices

Some parts of Pilo are derived from third-party open-source software, including Lody. See [THIRD_PARTY_NOTICES.md](./THIRD_PARTY_NOTICES.md) for source and license details.

## License

Pilo is licensed under the [Apache License 2.0](./LICENSE).

Third-party source and attribution notices are listed in [THIRD_PARTY_NOTICES.md](./THIRD_PARTY_NOTICES.md).
