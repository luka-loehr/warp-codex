use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexReasoningOption {
    pub effort: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexModelEntry {
    pub id: String,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub default_effort: String,
    pub supported_efforts: Vec<CodexReasoningOption>,
    pub vision_supported: bool,
    pub is_default: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CodexWindowQuota {
    pub used_percent: f64,
    pub window_mins: Option<u64>,
    pub resets_at_unix: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CodexSnapshot {
    pub cli_path: Option<PathBuf>,
    pub cli_version: Option<String>,
    pub logged_in: bool,
    pub requires_auth: bool,
    pub auth_mode: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub five_hour: Option<CodexWindowQuota>,
    pub weekly: Option<CodexWindowQuota>,
    pub credits_balance: Option<String>,
    pub credits_unlimited: bool,
    pub rate_limit_reached: bool,
    pub lifetime_tokens: Option<u64>,
    pub peak_daily_tokens: Option<u64>,
    pub models: Vec<CodexModelEntry>,
    pub last_error: Option<String>,
}

impl CodexSnapshot {
    pub fn plan_badge(&self) -> String {
        if !self.logged_in {
            if self.cli_path.is_none() {
                return "Codex CLI Not Found".to_owned();
            }
            return "Signed Out".to_owned();
        }
        match self.plan_type.as_deref() {
            Some("pro") => "ChatGPT Pro".to_owned(),
            Some("promax") => "ChatGPT Pro Max".to_owned(),
            Some("prolite") => "ChatGPT Pro Lite".to_owned(),
            Some("plus") => "ChatGPT Plus".to_owned(),
            Some("team") => "ChatGPT Team".to_owned(),
            Some("business")
            | Some("self_serve_business_prolite")
            | Some("self_serve_business_usage_based") => "ChatGPT Business".to_owned(),
            Some("enterprise")
            | Some("ent26")
            | Some("enterprise_cbp_automation")
            | Some("enterprise_cbp_usage_based") => "ChatGPT Enterprise".to_owned(),
            Some("edu") | Some("edu_plus") | Some("edu_pro") => "ChatGPT Edu".to_owned(),
            Some("go") => "ChatGPT Go".to_owned(),
            Some("free") => "ChatGPT Free".to_owned(),
            Some(other) if !other.is_empty() && other != "unknown" => format!("ChatGPT {other}"),
            _ => match self.auth_mode.as_deref() {
                Some("apiKey") | Some("apikey") => "OpenAI API Key".to_owned(),
                Some("amazonBedrock") => "Amazon Bedrock".to_owned(),
                _ => "Codex Connected".to_owned(),
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct ServerNotification {
    pub method: String,
    pub params: Value,
}

static SNAPSHOT: OnceLock<RwLock<CodexSnapshot>> = OnceLock::new();
static STATE_BROADCAST: OnceLock<(
    async_broadcast::Sender<()>,
    async_broadcast::InactiveReceiver<()>,
)> = OnceLock::new();
static CLIENT: OnceLock<Mutex<Option<Arc<CodexClient>>>> = OnceLock::new();

pub fn snapshot() -> CodexSnapshot {
    SNAPSHOT.get_or_init(|| RwLock::new(default_boot_snapshot())).read().clone()
}

pub fn subscribe_state_changes() -> async_broadcast::Receiver<()> {
    let (_, rx) = STATE_BROADCAST.get_or_init(|| {
        let (mut tx, rx) = async_broadcast::broadcast(64);
        tx.set_overflow(true);
        (tx, rx.deactivate())
    });
    rx.activate_cloned()
}

fn notify_state_changed() {
    if let Some((tx, _)) = STATE_BROADCAST.get() {
        let _ = tx.try_broadcast(());
    }
}

pub fn update_snapshot(mutator: impl FnOnce(&mut CodexSnapshot)) {
    {
        let lock = SNAPSHOT.get_or_init(|| RwLock::new(default_boot_snapshot()));
        let mut guard = lock.write();
        mutator(&mut guard);
    }
    notify_state_changed();
}

fn default_boot_snapshot() -> CodexSnapshot {
    let cli_path = find_codex_binary();
    let cli_version = cli_path.as_deref().and_then(read_codex_version);
    CodexSnapshot {
        cli_path,
        cli_version,
        models: fallback_codex_models(),
        ..CodexSnapshot::default()
    }
}

pub fn fallback_codex_models() -> Vec<CodexModelEntry> {
    let efforts = vec![
        CodexReasoningOption {
            effort: "low".to_owned(),
            description: "Fast responses with light reasoning".to_owned(),
        },
        CodexReasoningOption {
            effort: "medium".to_owned(),
            description: "Balanced speed and reasoning depth".to_owned(),
        },
        CodexReasoningOption {
            effort: "high".to_owned(),
            description: "Deep multi-step reasoning".to_owned(),
        },
        CodexReasoningOption {
            effort: "xhigh".to_owned(),
            description: "Maximum reasoning effort for complex tasks".to_owned(),
        },
    ];
    vec![
        CodexModelEntry {
            id: "gpt-5.4".to_owned(),
            slug: "gpt-5.4".to_owned(),
            display_name: "GPT-5.4".to_owned(),
            description: "Latest frontier coding & agentic model".to_owned(),
            default_effort: "medium".to_owned(),
            supported_efforts: efforts.clone(),
            vision_supported: true,
            is_default: true,
        },
        CodexModelEntry {
            id: "gpt-5.3-codex".to_owned(),
            slug: "gpt-5.3-codex".to_owned(),
            display_name: "GPT-5.3 Codex".to_owned(),
            description: "Agentic software engineering specialist".to_owned(),
            default_effort: "medium".to_owned(),
            supported_efforts: efforts,
            vision_supported: true,
            is_default: false,
        },
    ]
}

pub fn find_codex_binary() -> Option<PathBuf> {
    for env_key in ["WARP_CODEX_BIN", "CODEX_BIN"] {
        if let Ok(val) = std::env::var(env_key) {
            let p = PathBuf::from(val.trim());
            if is_executable(&p) {
                return Some(p);
            }
        }
    }

    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("codex");
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }

    let home = dirs::home_dir();
    let mut candidates = vec![
        PathBuf::from("/opt/homebrew/bin/codex"),
        PathBuf::from("/usr/local/bin/codex"),
        PathBuf::from("/opt/kiste/bin/codex"),
        PathBuf::from("/usr/bin/codex"),
    ];
    if let Some(ref h) = home {
        candidates.push(h.join(".local/bin/codex"));
        candidates.push(h.join(".npm-global/bin/codex"));
        candidates.push(h.join(".bun/bin/codex"));
        candidates.push(h.join(".cargo/bin/codex"));
        candidates.push(h.join(".bots/engines/codex/node_modules/.bin/codex"));
        if let Ok(entries) = std::fs::read_dir(h.join(".nvm/versions/node")) {
            let mut nvm_bins: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path().join("bin/codex"))
                .filter(|p| is_executable(p))
                .collect();
            nvm_bins.sort();
            if let Some(newest) = nvm_bins.pop() {
                candidates.insert(0, newest);
            }
        }
    }
    for c in candidates {
        if is_executable(&c) {
            return Some(c);
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = Command::new("/bin/zsh")
            .args(["-lc", "command -v codex"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            && out.status.success()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(line) = text.lines().rfind(|l| !l.trim().is_empty()) {
                let p = PathBuf::from(line.trim());
                if is_executable(&p) {
                    return Some(p);
                }
            }
        }
    }

    None
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| (m.permissions().mode() & 0o111) != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn enriched_path_env(bin: &Path) -> String {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(parent) = bin.parent()
        && !parent.as_os_str().is_empty()
    {
        dirs.push(parent.to_path_buf());
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    if let Some(h) = dirs::home_dir() {
        dirs.push(h.join(".local/bin"));
        dirs.push(h.join(".npm-global/bin"));
        dirs.push(h.join(".bun/bin"));
        dirs.push(h.join(".cargo/bin"));
        if let Ok(entries) = std::fs::read_dir(h.join(".nvm/versions/node")) {
            let mut nvm_bins: Vec<PathBuf> =
                entries.flatten().map(|e| e.path().join("bin")).collect();
            nvm_bins.sort();
            if let Some(newest) = nvm_bins.pop() {
                dirs.push(newest);
            }
        }
    }
    if let Ok(existing) = std::env::var("PATH") {
        dirs.extend(std::env::split_paths(&existing));
    } else {
        dirs.push(PathBuf::from("/usr/bin"));
        dirs.push(PathBuf::from("/bin"));
        dirs.push(PathBuf::from("/usr/sbin"));
        dirs.push(PathBuf::from("/sbin"));
    }
    std::env::join_paths(dirs)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin".to_owned())
}

fn read_codex_version(bin: &Path) -> Option<String> {
    let out = Command::new(bin)
        .arg("--version")
        .env("PATH", enriched_path_env(bin))
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .ok()?;
    let combined = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr).into_owned()
    } else {
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let line = combined.lines().next()?.trim().to_owned();
    (!line.is_empty()).then_some(line)
}

pub struct CodexClient {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, async_channel::Sender<Result<Value, String>>>>>,
    notifications: async_broadcast::InactiveReceiver<ServerNotification>,
    alive: Arc<std::sync::atomic::AtomicBool>,
}

impl CodexClient {
    fn spawn_new() -> Result<Arc<Self>, String> {
        let bin = find_codex_binary().ok_or_else(|| {
            "Codex CLI (`codex`) was not found on PATH. Install it with `npm i -g @openai/codex` or `brew install codex`.".to_owned()
        })?;
        let version = read_codex_version(&bin);
        update_snapshot(|snap| {
            snap.cli_path = Some(bin.clone());
            if version.is_some() {
                snap.cli_version = version;
            }
        });

        let mut cmd = Command::new(&bin);
        cmd.arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Ensure Homebrew/nvm/local paths are on PATH even when launched from macOS Finder/Dock.
        cmd.env("PATH", enriched_path_env(&bin));

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to start `{} app-server`: {e}", bin.display()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Failed to open stdin for `codex app-server`".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to open stdout for `codex app-server`".to_owned())?;
        let stderr = child.stderr.take();

        let pending: Arc<Mutex<HashMap<u64, async_channel::Sender<Result<Value, String>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (mut notif_tx, notif_rx) = async_broadcast::broadcast(512);
        notif_tx.set_overflow(true);
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let client = Arc::new(Self {
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            next_id: AtomicU64::new(1),
            pending: pending.clone(),
            notifications: notif_rx.deactivate(),
            alive: alive.clone(),
        });

        if let Some(err_pipe) = stderr {
            std::thread::Builder::new()
                .name("codex-app-server-stderr".to_owned())
                .spawn(move || {
                    let reader = BufReader::new(err_pipe);
                    for line in reader.lines().map_while(Result::ok) {
                        if !line.trim().is_empty() {
                            log::debug!("codex app-server stderr: {line}");
                        }
                    }
                })
                .ok();
        }

        let client_weak = Arc::downgrade(&client);
        std::thread::Builder::new()
            .name("codex-app-server-stdout".to_owned())
            .spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    let Ok(raw) = line else {
                        break;
                    };
                    let trimmed = raw.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
                        log::debug!("codex non-json stdout: {trimmed}");
                        continue;
                    };

                    // 1. Server-to-client request (has both `id` and `method`)
                    if let (Some(req_id), Some(method)) =
                        (msg.get("id").cloned(), msg.get("method").and_then(Value::as_str))
                    {
                        if let Some(strong) = client_weak.upgrade() {
                            handle_server_request(&strong, req_id, method, &msg["params"]);
                        }
                        continue;
                    }

                    // 2. Response or error to one of our requests (has numeric `id`, no `method`)
                    if let Some(id) = msg.get("id").and_then(Value::as_u64) {
                        let sender = pending.lock().ok().and_then(|mut map| map.remove(&id));
                        if let Some(tx) = sender {
                            if let Some(err_obj) = msg.get("error") {
                                let err_msg = err_obj
                                    .get("message")
                                    .and_then(Value::as_str)
                                    .unwrap_or("Codex RPC error")
                                    .to_owned();
                                let _ = tx.send_blocking(Err(err_msg));
                            } else {
                                let _ = tx.send_blocking(Ok(
                                    msg.get("result").cloned().unwrap_or(Value::Null),
                                ));
                            }
                        }
                        continue;
                    }

                    // 3. Server notification (`method` without `id`)
                    if let Some(method) = msg.get("method").and_then(Value::as_str) {
                        let params = msg.get("params").cloned().unwrap_or(Value::Null);
                        apply_global_notification(method, &params);
                        let _ = notif_tx.try_broadcast(ServerNotification {
                            method: method.to_owned(),
                            params,
                        });
                    }
                }

                alive.store(false, Ordering::SeqCst);
                if let Ok(mut map) = pending.lock() {
                    for (_, tx) in map.drain() {
                        let _ = tx.send_blocking(Err(
                            "Codex app-server connection closed unexpectedly".to_owned(),
                        ));
                    }
                }
            })
            .map_err(|e| format!("Failed to spawn Codex stdout reader thread: {e}"))?;

        Ok(client)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub fn subscribe(&self) -> async_broadcast::Receiver<ServerNotification> {
        self.notifications.activate_cloned()
    }

    pub fn write_raw(&self, payload: &Value) -> Result<(), String> {
        let mut line = serde_json::to_string(payload).map_err(|e| e.to_string())?;
        line.push('\n');
        let mut guard = self
            .stdin
            .lock()
            .map_err(|_| "Codex stdin mutex poisoned".to_owned())?;
        guard
            .write_all(line.as_bytes())
            .and_then(|_| guard.flush())
            .map_err(|e| {
                self.alive.store(false, Ordering::SeqCst);
                format!("Failed writing to `codex app-server`: {e}")
            })
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = async_channel::bounded(1);
        {
            let mut map = self
                .pending
                .lock()
                .map_err(|_| "Codex pending map mutex poisoned".to_owned())?;
            map.insert(id, tx);
        }
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(err) = self.write_raw(&req) {
            if let Ok(mut map) = self.pending.lock() {
                map.remove(&id);
            }
            return Err(err);
        }
        rx.recv()
            .await
            .map_err(|_| "Codex app-server closed before replying".to_owned())?
    }
}

impl Drop for CodexClient {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn handle_server_request(client: &CodexClient, id: Value, method: &str, params: &Value) {
    let result = match method {
        "item/commandExecution/requestApproval"
        | "item/fileChange/requestApproval"
        | "execCommandApproval"
        | "applyPatchApproval" => json!({ "decision": "accept" }),
        "item/permissions/requestApproval" => json!({
            "permissions": params.get("permissions").cloned().unwrap_or_else(|| json!({})),
            "scope": "session",
        }),
        "item/tool/requestUserInput" => json!({ "answers": {} }),
        _ => {
            let _ = client.write_raw(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Unsupported client callback: {method}"),
                }
            }));
            return;
        }
    };
    let _ = client.write_raw(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }));
}

fn apply_global_notification(method: &str, params: &Value) {
    match method {
        "account/rateLimits/updated" => {
            if let Some(rl) = params.get("rateLimits") {
                update_snapshot(|snap| merge_rate_limits_into_snapshot(snap, rl));
            }
        }
        "account/updated" => {
            let auth_mode = params.get("authMode").and_then(Value::as_str).map(str::to_owned);
            let plan_type = params.get("planType").and_then(Value::as_str).map(str::to_owned);
            update_snapshot(|snap| {
                snap.logged_in = auth_mode.is_some();
                snap.auth_mode = auth_mode;
                if plan_type.is_some() {
                    snap.plan_type = plan_type;
                }
            });
            spawn_background_refresh();
        }
        "account/login/completed" => {
            let success = params.get("success").and_then(Value::as_bool).unwrap_or(false);
            if !success {
                let err = params
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                update_snapshot(|snap| snap.last_error = err);
            }
            spawn_background_refresh();
        }
        _ => {}
    }
}

pub async fn get_or_connect() -> Result<Arc<CodexClient>, String> {
    let existing = {
        let slot = CLIENT.get_or_init(|| Mutex::new(None));
        let guard = slot.lock().map_err(|_| "Codex client lock poisoned".to_owned())?;
        guard.clone().filter(|c| c.is_alive())
    };
    if let Some(client) = existing {
        return Ok(client);
    }

    let client = blocking::unblock(CodexClient::spawn_new).await?;
    let init_params = json!({
        "clientInfo": {
            "name": "warp-codex",
            "title": "Warp Codex",
            "version": "0.1.0",
        },
        "capabilities": {
            "experimentalApi": true,
            "requestAttestation": false,
        }
    });
    client.call("initialize", init_params).await?;
    client.write_raw(&json!({ "method": "initialized" }))?;

    {
        let slot = CLIENT.get_or_init(|| Mutex::new(None));
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(client.clone());
        }
    }
    Ok(client)
}

pub fn spawn_background_refresh() {
    std::thread::Builder::new()
        .name("codex-refresh".to_owned())
        .spawn(|| {
            let _ = futures::executor::block_on(refresh_all());
        })
        .ok();
}

pub async fn refresh_all() -> Result<CodexSnapshot, String> {
    let client = match get_or_connect().await {
        Ok(c) => c,
        Err(err) => {
            update_snapshot(|snap| {
                snap.last_error = Some(err.clone());
            });
            return Err(err);
        }
    };

    // 1. Account & auth check (`account/read`)
    if let Ok(acc_res) = client.call("account/read", json!({ "refreshToken": false })).await {
        let requires_auth = acc_res
            .get("requiresOpenaiAuth")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let account = acc_res.get("account").filter(|v| !v.is_null());
        update_snapshot(|snap| {
            snap.requires_auth = requires_auth;
            if let Some(acc) = account {
                snap.logged_in = true;
                snap.auth_mode = acc.get("type").and_then(Value::as_str).map(str::to_owned);
                snap.email = acc.get("email").and_then(Value::as_str).map(str::to_owned);
                if let Some(pt) = acc.get("planType").and_then(Value::as_str) {
                    snap.plan_type = Some(pt.to_owned());
                }
                snap.last_error = None;
            } else {
                snap.logged_in = !requires_auth;
                snap.auth_mode = None;
                snap.email = None;
            }
        });
    }

    // 2. Models (`model/list` across pages)
    let mut parsed_models = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..8 {
        let res = client
            .call(
                "model/list",
                json!({
                    "cursor": cursor,
                    "limit": 50,
                    "includeHidden": false,
                }),
            )
            .await;
        let Ok(page) = res else {
            break;
        };
        if let Some(arr) = page.get("data").and_then(Value::as_array) {
            for item in arr {
                if item.get("hidden").and_then(Value::as_bool).unwrap_or(false) {
                    continue;
                }
                if let Some(entry) = parse_model_entry(item) {
                    parsed_models.push(entry);
                }
            }
        }
        cursor = page
            .get("nextCursor")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    if !parsed_models.is_empty() {
        update_snapshot(|snap| {
            snap.models = parsed_models;
        });
    }

    // 3. Native Plan & Rate Limits (`account/rateLimits/read`)
    if let Ok(rl_res) = client
        .call(
            "account/rateLimits/read",
            json!({ "excludeResetCreditDetails": true }),
        )
        .await
        && let Some(rl) = rl_res.get("rateLimits")
    {
        update_snapshot(|snap| merge_rate_limits_into_snapshot(snap, rl));
    }

    // 4. Token Usage Summary (`account/usage/read`)
    if let Ok(usage_res) = client.call("account/usage/read", json!({})).await
        && let Some(summary) = usage_res.get("summary")
    {
        let lifetime = summary.get("lifetimeTokens").and_then(Value::as_u64);
        let peak = summary.get("peakDailyTokens").and_then(Value::as_u64);
        update_snapshot(|snap| {
            if lifetime.is_some() {
                snap.lifetime_tokens = lifetime;
            }
            if peak.is_some() {
                snap.peak_daily_tokens = peak;
            }
        });
    }

    Ok(snapshot())
}

pub(crate) fn parse_model_entry(item: &Value) -> Option<CodexModelEntry> {
    let slug = item
        .get("model")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)?
        .trim()
        .to_owned();
    if slug.is_empty() {
        return None;
    }
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(&slug)
        .to_owned();
    let display_name = item
        .get("displayName")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&slug)
        .to_owned();
    let description = item
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let default_effort = item
        .get("defaultReasoningEffort")
        .and_then(Value::as_str)
        .unwrap_or("medium")
        .to_owned();
    let supported_efforts = item
        .get("supportedReasoningEfforts")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|opt| {
                    let effort = opt.get("reasoningEffort")?.as_str()?.to_owned();
                    let desc = opt
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    Some(CodexReasoningOption {
                        effort,
                        description: desc,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let vision_supported = item
        .get("inputModalities")
        .and_then(Value::as_array)
        .is_none_or(|mods| mods.iter().any(|m| m.as_str() == Some("image")));
    let is_default = item.get("isDefault").and_then(Value::as_bool).unwrap_or(false);

    Some(CodexModelEntry {
        id,
        slug,
        display_name,
        description,
        default_effort,
        supported_efforts,
        vision_supported,
        is_default,
    })
}

pub(crate) fn merge_rate_limits_into_snapshot(snap: &mut CodexSnapshot, rl: &Value) {
    if let Some(pt) = rl.get("planType").and_then(Value::as_str)
        && !pt.is_empty()
    {
        snap.plan_type = Some(pt.to_owned());
    }
    if let Some(primary) = rl.get("primary").filter(|v| !v.is_null())
        && let Some(used) = primary.get("usedPercent").and_then(Value::as_f64)
    {
        snap.five_hour = Some(CodexWindowQuota {
            used_percent: used.clamp(0.0, 100.0),
            window_mins: primary.get("windowDurationMins").and_then(Value::as_u64),
            resets_at_unix: primary.get("resetsAt").and_then(Value::as_i64),
        });
    }
    if let Some(secondary) = rl.get("secondary").filter(|v| !v.is_null())
        && let Some(used) = secondary.get("usedPercent").and_then(Value::as_f64)
    {
        snap.weekly = Some(CodexWindowQuota {
            used_percent: used.clamp(0.0, 100.0),
            window_mins: secondary.get("windowDurationMins").and_then(Value::as_u64),
            resets_at_unix: secondary.get("resetsAt").and_then(Value::as_i64),
        });
    }
    if let Some(credits) = rl.get("credits").filter(|v| !v.is_null()) {
        snap.credits_unlimited = credits
            .get("unlimited")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Some(bal) = credits.get("balance").and_then(Value::as_str) {
            snap.credits_balance = Some(bal.to_owned());
        }
    }
    let reached_by_flag = rl
        .get("rateLimitReachedType")
        .is_some_and(|v| !v.is_null());
    let reached_by_pct = snap
        .five_hour
        .as_ref()
        .is_some_and(|w| w.used_percent >= 100.0)
        || snap
            .weekly
            .as_ref()
            .is_some_and(|w| w.used_percent >= 100.0);
    snap.rate_limit_reached = reached_by_flag || reached_by_pct;
}

pub async fn start_chatgpt_login() -> Result<String, String> {
    let client = get_or_connect().await?;
    let res = client
        .call(
            "account/login/start",
            json!({
                "type": "chatgpt",
                "appBrand": "codex",
                "useHostedLoginSuccessPage": true,
            }),
        )
        .await?;
    res.get("authUrl")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "Codex login did not return an authorization URL".to_owned())
}

pub async fn start_device_code_login() -> Result<(String, String), String> {
    let client = get_or_connect().await?;
    let res = client
        .call("account/login/start", json!({ "type": "chatgptDeviceCode" }))
        .await?;
    let url = res
        .get("verificationUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing verificationUrl in device-code response".to_owned())?
        .to_owned();
    let code = res
        .get("userCode")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing userCode in device-code response".to_owned())?
        .to_owned();
    Ok((url, code))
}

pub async fn logout() -> Result<(), String> {
    let client = get_or_connect().await?;
    client.call("account/logout", json!({})).await?;
    let _ = refresh_all().await;
    Ok(())
}

/// Runs a lightweight, read-only `ephemeral: true` turn on `codex app-server` for inline Warp AI
/// features (command search, ghost-text suggestions, commit messages, block titles, dialogue).
pub async fn run_ephemeral_turn(
    prompt: &str,
    cwd: Option<&Path>,
    model: Option<&str>,
    effort: Option<&str>,
    output_schema: Option<Value>,
) -> Result<String, String> {
    let client = get_or_connect().await?;
    let mut rx = client.subscribe();

    let cwd_str = cwd
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "/tmp".to_owned());

    let mut start_params = json!({
        "cwd": cwd_str,
        "approvalPolicy": "never",
        "sandbox": "read-only",
        "ephemeral": true,
    });
    if let Some(m) = model.filter(|m| !m.is_empty() && *m != "auto") {
        start_params["model"] = Value::String(m.to_owned());
    }

    let thread_res = client.call("thread/start", start_params).await?;
    let thread_id = thread_res["thread"]["id"]
        .as_str()
        .ok_or_else(|| "Codex thread/start returned no thread id".to_owned())?
        .to_owned();

    let mut turn_params = json!({
        "threadId": thread_id,
        "cwd": cwd_str,
        "approvalPolicy": "never",
        "sandboxPolicy": {
            "type": "readOnly",
            "networkAccess": false,
        },
        "input": [{
            "type": "text",
            "text": prompt,
            "text_elements": [],
        }],
    });
    if let Some(eff) = effort.filter(|e| !e.is_empty()) {
        turn_params["effort"] = Value::String(eff.to_owned());
    }
    if let Some(schema) = output_schema {
        turn_params["outputSchema"] = schema;
    }

    let turn_res = client.call("turn/start", turn_params).await?;
    let turn_id = turn_res["turn"]["id"].as_str().unwrap_or_default().to_owned();

    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    let mut streamed = String::new();
    let mut completed_text: Option<String> = None;

    while std::time::Instant::now() < deadline {
        let Ok(notif) = rx.recv().await else {
            break;
        };
        let p_tid = notif.params["threadId"].as_str().unwrap_or_default();
        if p_tid != thread_id {
            continue;
        }
        match notif.method.as_str() {
            "item/agentMessage/delta" => {
                if let Some(delta) = notif.params["delta"].as_str() {
                    streamed.push_str(delta);
                }
            }
            "item/completed" => {
                let item = &notif.params["item"];
                if item["type"].as_str() == Some("agentMessage")
                    && let Some(text) = item["text"].as_str()
                {
                    completed_text = Some(text.to_owned());
                }
            }
            "turn/completed" => {
                let t = &notif.params["turn"];
                if !turn_id.is_empty()
                    && let Some(done_id) = t["id"].as_str()
                    && done_id != turn_id
                {
                    continue;
                }
                if t["status"].as_str() == Some("failed") {
                    let msg = t["error"]["message"]
                        .as_str()
                        .unwrap_or("Codex turn failed")
                        .to_owned();
                    return Err(msg);
                }
                break;
            }
            _ => {}
        }
    }

    let answer = completed_text.unwrap_or(streamed);
    Ok(answer.trim().to_owned())
}
