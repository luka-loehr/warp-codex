<div align="center">
  <h1>WarpCodex</h1>
  <p><strong>Local-first Warp terminal for macOS — 100% powered by your local Codex CLI, zero telemetry, zero cloud walls.</strong></p>
  <p>
    <a href="#features">Features</a> ·
    <a href="#how-it-works">How It Works</a> ·
    <a href="#install">Install</a> ·
    <a href="#build-from-source">Build from Source</a> ·
    <a href="#license">License</a>
  </p>
</div>

---

`warp-codex` is a clean, local-first build of the open-source [Warp terminal](https://github.com/warpdotdev/warp) where every single AI surface runs directly on your machine through your installed [`codex` CLI](https://github.com/openai/codex) (`codex app-server` over stdio JSON-RPC 2.0) — using your own ChatGPT Plus / Pro / Team subscription or API key.

No Warp account. No Warp servers. No RudderStack telemetry. No Sentry crash uploads. No cloud sync or background auto-updaters.

---

## Features

- **100% Native Warp Terminal + Full AI Parity**
  - **Agent Mode (`generate_multi_agent_output`)** — live token-by-token streaming, extended reasoning blocks, live shell command execution & output, file diffs, MCP tool calls, web search citations, and live todo/plan checklists (`turn/plan/updated`), backed by persistent `codex app-server` threads (`thread/start` & `thread/resume`).
  - **Plan Mode** — native collaboration plan mode wired straight to Codex `collaborationMode: "plan"`.
  - **All Secondary AI Features Included** — `#` natural-language command search, Warp AI dialogue panel, workflow metadata & argument generation, passive error-fix & next-command suggestions, Agent Mode query autocomplete, `@` relevant-file ranking, shared block title generation, and git code-review commit/PR descriptions.
- **Dynamic Models & Reasoning Efforts from Your Plan**
  - Queries `model/list` directly from your local `codex app-server` and exposes every model and reasoning effort tier (`Low`, `Medium`, `High`, `Extra High`, `Max`, `Ultra`) available on your account right inside Warp's native model picker.
- **Native Codex Plan & Rolling Quota UI**
  - Displays your live ChatGPT/Codex plan badge (`Plus`, `Pro`, `Team`, etc.), connected account email, **5-hour rolling window** usage bar + reset countdown, **7-day rolling window** usage bar + reset countdown, and lifetime token stats directly inside:
    - The Agent Mode footer **Usage Popover**
    - **Settings → Billing and Usage** (with one-click ChatGPT OAuth sign-in, refresh, and sign-out)
- **Zero Telemetry & Zero Cloud Dependencies**
  - RudderStack event batching and disk persistence are stripped to in-memory no-ops.
  - Sentry (`cocoa_sentry` / `crash_reporting`), Firebase anonymous auth, cloud preference syncing, experiment polling, and background auto-update checks are disabled.
  - Opens straight into your local shell on first launch with zero sign-up modal.

---

## How It Works

```text
┌──────────────────────────────────────────────────────────────────┐
│                        WarpCodex.app (macOS)                     │
│  ┌────────────────────┐  ┌───────────────────┐  ┌──────────────┐ │
│  │ Agent Mode & Tools │  │ # Command / AI    │  │ Usage Popover│ │
│  │ (Blocks, Diffs,    │  │ Suggestions, Code │  │ & Billing    │ │
│  │  Plans, Reasoning) │  │ Review, Workflows │  │ Settings UI  │ │
│  └─────────┬──────────┘  └─────────┬─────────┘  └──────┬───────┘ │
│            └───────────────────────┼───────────────────┘         │
│                                    ▼                             │
│                   app/src/ai/codex_local/{rpc,agent_stream,ui}   │
└────────────────────────────────────┬─────────────────────────────┘
                                     │ stdio JSONL (JSON-RPC 2.0)
                                     ▼
                       codex app-server (local binary)
             (/opt/homebrew/bin/codex · ~/.nvm · PATH · CODEX_BIN)
```

1. On startup, `WarpCodex` locates your local `codex` executable (`WARP_CODEX_BIN` / `CODEX_BIN`, `PATH`, `/opt/homebrew/bin/codex`, `/usr/local/bin/codex`, `~/.local/bin/codex`, `~/.nvm`, or login shell) and starts a persistent `codex app-server` child process over stdio.
2. It runs the standard `initialize` → `initialized` handshake and queries `account/read`, `model/list`, `account/rateLimits/read`, and `account/usage/read`, subscribing to live `account/rateLimits/updated` and `account/login/completed` notifications.
3. If you are already signed in via `codex login`, `WarpCodex` picks up your session immediately. Otherwise, you can sign in directly from **Settings → Billing and Usage** (via `account/login/start` ChatGPT OAuth) or run `codex login` in any terminal tab.

---

## Install

### Prerequisites

Install the official OpenAI Codex CLI on your Mac and sign in once:

```bash
brew install codex
# or: npm install -g @openai/codex

codex login
```

### Download Release (`.dmg` / `.app`)

1. Download **`WarpCodex.dmg`** from the latest [GitHub Release](../../releases).
2. Drag **`WarpCodex.app`** to `/Applications`.
3. Since OSS builds use ad-hoc signing by default, clear macOS Gatekeeper quarantine on first download if prompted:
   ```bash
   xattr -cr /Applications/WarpCodex.app
   ```
4. Launch **WarpCodex** — your terminal and local Codex agent are ready immediately.

---

## Build from Source

### Requirements (macOS Apple Silicon)

- Xcode (with Metal toolchain: `xcodebuild -downloadComponent MetalToolchain` if needed)
- Rust toolchain matching [`rust-toolchain.toml`](rust-toolchain.toml) (`rustup show`)
- Homebrew packages & `cargo-bundle`:
  ```bash
  brew install git-lfs jq pkgconf llvm protobuf create-dmg
  git lfs install && git lfs pull
  cargo install cargo-bundle --version 0.11.0 --locked
  ```

### Build `.app` and `.dmg`

```bash
NO_LICENSES=1 ./script/bundle --channel oss --arch aarch64 --selfsign
```

Build outputs are written to:
- `target/release-lto/bundle/osx/WarpCodex.app`
- `target/release-lto/bundle/osx/WarpCodex.dmg`

---

## License

`warp-codex` is a derivative work of [`warpdotdev/warp`](https://github.com/warpdotdev/warp) and preserves upstream licensing in full:

- Core terminal & application crates (`app/`, `crates/`): **[GNU Affero General Public License v3.0 (AGPL-3.0)](LICENSE-AGPL)**
- UI framework crates (`warpui/`): **[MIT License](LICENSE-MIT)**
