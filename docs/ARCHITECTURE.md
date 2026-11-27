# Architecture & Building from Source

## Overview

`WarpCodex` replaces Warp's cloud GraphQL/gRPC multi-agent client, cloud authentication, and telemetry pipelines with a self-contained local adapter in [`app/src/ai/codex_local/`](../app/src/ai/codex_local):

```text
┌────────────────────────────────────────────────────────────────────┐
│                         WarpCodex.app (macOS)                      │
│  ┌─────────────────────┐  ┌────────────────────┐  ┌──────────────┐ │
│  │ Agent Mode & Tools  │  │ # Command / AI     │  │ Usage Popover│ │
│  │ (Blocks, Diffs,     │  │ Suggestions, Code  │  │ & Billing    │ │
│  │  Plans, Reasoning)  │  │ Review, Workflows  │  │ Settings UI  │ │
│  └──────────┬──────────┘  └─────────┬──────────┘  └──────┬───────┘ │
│             └───────────────────────┼────────────────────┘         │
│                                     ▼                              │
│              app/src/ai/codex_local/{rpc,agent_stream,ui,mod}      │
└─────────────────────────────────────┬──────────────────────────────┘
                                      │ stdio JSONL (JSON-RPC 2.0)
                                      ▼
                         codex app-server (local binary)
               (/opt/homebrew/bin/codex · ~/.nvm · PATH · CODEX_BIN)
```

### Module Layout (`app/src/ai/codex_local/`)

| File | Responsibility |
| --- | --- |
| `rpc.rs` | Persistent `codex app-server` child-process lifecycle, `PATH` discovery (`WARP_CODEX_BIN` / `CODEX_BIN`, `/opt/homebrew/bin`, `/usr/local/bin`, `~/.local/bin`, `~/.nvm`, login shell), `initialize` / `initialized` JSON-RPC 2.0 handshake, bidirectional server-request approval handling, `account/read`, `model/list`, `account/rateLimits/read`, `account/usage/read`, live `account/rateLimits/updated` and `account/login/completed` listeners, ChatGPT OAuth `account/login/start` / `account/logout`, and ephemeral one-shot turns. |
| `agent_stream.rs` | Maps Warp `RequestParams` (`AIAgentInput`, `AIAgentContext` including terminal blocks, selections, file trees, and base64 images, `AIAgentAttachment`, Plan mode `collaborationMode`, `model::effort` tiers, and cancellation via `turn/interrupt`) into live `warp_multi_agent_api::ResponseEvent` frames (`StreamInit`, `CreateTask`, `AgentOutput` and `AgentReasoning` deltas, `UpdateTodos`, `ToolCall` / `ToolCallResult` for shell, file diffs, MCP, and web search, `RequestMetadata`, and `StreamFinished`). |
| `ui.rs` | Native Warp UI components rendering your ChatGPT/Codex plan badge, connected email, 5-hour and 7-day rolling rate-limit bars, reset countdowns, and lifetime token totals inside `UsagePopoverView` and `Settings → Billing and Usage`. |
| `mod.rs` | `CodexSyncModel` (`SingletonEntity`) reactive sync into `ApiKeyManager`, `LLMPreferences`, and `AIRequestUsageModel`; dynamic `ModelsByFeature` generation across `Low` through `Ultra` reasoning efforts; and secondary AI endpoints (`#` command translation, AI assistant panel, workflow generation, passive error/next-command suggestions, `@` file ranking, shared block titles, and Git code review). |

### Telemetry & Cloud Removal

- **RudderStack telemetry** (`app/src/server/telemetry/mod.rs`): Event batching, disk persistence, and network flushes are replaced with an inert drain channel.
- **Crash reporting & cloud sync** (`crates/warp_features/src/lib.rs`, `app/src/settings/cloud_preferences_syncer.rs`, `app/src/lib.rs`): `cocoa_sentry` / `CrashReporting`, `Autoupdate`, `Changelog`, Firebase startup auth, cloud preference syncing, and account-first onboarding gates are disabled on `Channel::Oss`.
- **Network endpoints** (`app/src/bin/oss.rs`): `WarpServerConfig` and `OzConfig` point exclusively to inert loopback (`127.0.0.1:9`) with `prewarmed_sandbox_control_url: None`.

---

## Building from Source (macOS Apple Silicon)

### Prerequisites

1. **Xcode** with Command Line Tools and Metal toolchain (`xcodebuild -downloadComponent MetalToolchain` if prompted).
2. **Rust toolchain** matching [`rust-toolchain.toml`](../rust-toolchain.toml) (`rustup show`).
3. **System dependencies & `cargo-bundle`**:
   ```bash
   brew install git-lfs jq pkgconf llvm protobuf create-dmg
   git lfs install && git lfs pull
   cargo install cargo-bundle --version 0.11.0 --locked
   ```

### Run Unit Tests

```bash
cargo test -p warp --lib ai::codex_local
```

### Build Signed `.app` and `.dmg`

```bash
NO_LICENSES=1 ./script/bundle --channel oss --arch aarch64 --selfsign
```

Artifacts are written to:
- `target/release-lto/bundle/osx/WarpCodex.app`
- `target/release-lto/bundle/osx/WarpCodex.dmg`
