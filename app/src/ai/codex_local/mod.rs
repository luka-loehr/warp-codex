pub mod agent_stream;
pub mod rpc;
pub mod ui;

use std::collections::HashMap;

use ::ai::api_keys::{ApiKeyManager, ChatGPTConnection, ChatGPTConnectionStatus};
use ::ai::llm_provider::LLMProvider;
use chrono::{TimeZone, Utc};
use serde::Deserialize;
use serde_json::json;
use warp_cli::agent::Harness;
use warp_graphql::scalars::time::ServerTimestamp;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

pub use agent_stream::generate_codex_multi_agent_output;
pub use rpc::{
    CodexSnapshot, logout, refresh_all, run_ephemeral_turn, snapshot, spawn_background_refresh,
    start_chatgpt_login, subscribe_state_changes,
};

use crate::ai::credit_availability::{AICreditAvailability, AICreditDenialReason, AICreditSource};
use crate::ai::generate_block_title::api::{GenerateBlockTitleRequest, GenerateBlockTitleResponse};
use crate::ai::generate_code_review_content::api::{
    GenerateCodeReviewContentRequest, GenerateCodeReviewContentResponse, OutputType,
};
use crate::ai::get_relevant_files::api::{GetRelevantFiles, GetRelevantFilesResponse};
use crate::ai::harness_availability::{HarnessAvailability, HarnessModelInfo};
use crate::ai::llms::{
    AvailableLLMs, LLMContextWindow, LLMId, LLMInfo, LLMPreferences, LLMUsageMetadata,
    ModelsByFeature,
};
use crate::ai::predict::generate_ai_input_suggestions::{
    AgentModeSuggestionV2, GenerateAIInputSuggestionsRequest,
    GenerateAIInputSuggestionsResponseV2,
};
use crate::ai::predict::generate_am_query_suggestions::{
    GenerateAMQuerySuggestionsRequest, GenerateAMQuerySuggestionsResponse, SimpleQuery, Suggestion,
};
use crate::ai::predict::predict_am_queries::{PredictAMQueriesRequest, PredictAMQueriesResponse};
use crate::ai::request_usage_model::{
    AIRequestUsageModel, RequestLimitInfo, RequestLimitRefreshDuration, RequestUsageInfo,
};
use crate::ai_assistant::requests::GenerateDialogueResult;
use crate::ai_assistant::utils::TranscriptPart;
use crate::ai_assistant::{
    AIGeneratedCommand, AIGeneratedCommandParameter, GenerateCommandsFromNaturalLanguageError,
};
use crate::drive::workflows::ai_assist::{
    GeneratedArgument, GeneratedCommandMetadata, GeneratedCommandMetadataError,
};
use crate::workspaces::user_workspaces::HeadlessTeamScope;

const EFFORT_DELIMITER: &str = "::";

/// Initializes background `codex app-server` discovery and wires live Codex account, model, and
/// rate-limit updates directly into Warp's singleton state models.
pub struct CodexSyncModel;

impl Entity for CodexSyncModel {
    type Event = ();
}

impl SingletonEntity for CodexSyncModel {}

impl CodexSyncModel {
    fn new(ctx: &mut ModelContext<Self>) -> Self {
        ctx.spawn(
            async move {
                let _ = refresh_all().await;
            },
            |me, _, ctx| {
                sync_codex_state_into_app(ctx);
                ctx.emit(());
                me.rearm_listener(ctx);
            },
        );
        Self
    }

    fn rearm_listener(&mut self, ctx: &mut ModelContext<Self>) {
        let mut rx = subscribe_state_changes();
        ctx.spawn(
            async move { rx.recv().await.ok() },
            |me, received, ctx| {
                if received.is_some() {
                    sync_codex_state_into_app(ctx);
                    ctx.emit(());
                    me.rearm_listener(ctx);
                }
            },
        );
    }
}

pub fn init_and_warm(ctx: &mut AppContext) {
    ctx.add_singleton_model(CodexSyncModel::new);
}

pub fn spawn_background_login() {
    std::thread::Builder::new()
        .name("warp-codex-oauth".to_owned())
        .spawn(|| {
            if let Ok(url) = futures::executor::block_on(start_chatgpt_login()) {
                #[cfg(target_os = "macos")]
                let _ = std::process::Command::new("/usr/bin/open").arg(&url).spawn();
                #[cfg(not(target_os = "macos"))]
                let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
            }
        })
        .ok();
}

pub fn spawn_background_logout() {
    std::thread::Builder::new()
        .name("warp-codex-logout".to_owned())
        .spawn(|| {
            let _ = futures::executor::block_on(logout());
            let _ = futures::executor::block_on(refresh_all());
        })
        .ok();
}

pub fn sync_codex_state_into_app(ctx: &mut AppContext) {
    let snap = snapshot();
    let status = chatgpt_connection_from_snapshot(&snap);
    ApiKeyManager::handle(ctx).update(ctx, |mgr, ctx| {
        if snap.logged_in && mgr.chatgpt_oauth_pending() {
            mgr.resolve_chatgpt_oauth(Some(status), ctx);
        } else {
            mgr.set_chatgpt_connection_status(status, ctx);
        }
    });
    LLMPreferences::handle(ctx).update(ctx, |prefs, ctx| {
        prefs.refresh_available_models(&HeadlessTeamScope::Personal, ctx);
    });
    AIRequestUsageModel::handle(ctx).update(ctx, |usage, ctx| {
        usage.refresh_request_usage_async(ctx);
        usage.request_availability_refresh(ctx);
    });
}

pub fn split_model_and_effort(raw_id: &str) -> (Option<String>, Option<String>) {
    let trimmed = raw_id.trim();
    if trimmed.is_empty()
        || trimmed == "auto"
        || trimmed == "cli-agent-auto"
        || trimmed == "computer-use-agent-auto"
    {
        return (None, None);
    }
    if let Some((slug, effort)) = trimmed.split_once(EFFORT_DELIMITER) {
        let model = (!slug.is_empty()).then(|| slug.to_owned());
        let eff = (!effort.is_empty()).then(|| effort.to_owned());
        return (model, eff);
    }
    (Some(trimmed.to_owned()), None)
}

fn format_effort_label(effort: &str) -> &'static str {
    match effort {
        "none" => "No Reasoning",
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra High",
        "max" => "Max",
        "ultra" => "Ultra",
        _ => "Custom",
    }
}

pub fn models_by_feature_from_snapshot(snap: &CodexSnapshot) -> ModelsByFeature {
    let source = if snap.models.is_empty() {
        rpc::fallback_codex_models()
    } else {
        snap.models.clone()
    };

    let mut choices = Vec::new();
    let mut default_id: Option<LLMId> = None;

    for m in &source {
        let base_id = LLMId::from(m.slug.clone());
        if m.is_default && default_id.is_none() {
            default_id = Some(base_id.clone());
        }

        // Default entry for the model (uses its default reasoning effort)
        choices.push(LLMInfo {
            display_name: m.display_name.clone(),
            base_model_name: m.display_name.clone(),
            id: base_id,
            reasoning_level: (!m.default_effort.is_empty())
                .then(|| format_effort_label(&m.default_effort).to_owned()),
            usage_metadata: LLMUsageMetadata {
                request_multiplier: 1,
                credit_multiplier: None,
            },
            description: (!m.description.is_empty()).then(|| m.description.clone()),
            disable_reason: None,
            vision_supported: m.vision_supported,
            spec: None,
            provider: LLMProvider::OpenAI,
            host_configs: HashMap::new(),
            discount_percentage: None,
            context_window: LLMContextWindow::default(),
        });

        // Explicit reasoning-effort entries so users can pick any effort supported by their plan
        for opt in &m.supported_efforts {
            if opt.effort == m.default_effort {
                continue;
            }
            let variant_id = LLMId::from(format!("{}{EFFORT_DELIMITER}{}", m.slug, opt.effort));
            let eff_label = format_effort_label(&opt.effort);
            choices.push(LLMInfo {
                display_name: format!("{} ({eff_label})", m.display_name),
                base_model_name: m.display_name.clone(),
                id: variant_id,
                reasoning_level: Some(eff_label.to_owned()),
                usage_metadata: LLMUsageMetadata {
                    request_multiplier: 1,
                    credit_multiplier: None,
                },
                description: (!opt.description.is_empty()).then(|| opt.description.clone()),
                disable_reason: None,
                vision_supported: m.vision_supported,
                spec: None,
                provider: LLMProvider::OpenAI,
                host_configs: HashMap::new(),
                discount_percentage: None,
                context_window: LLMContextWindow::default(),
            });
        }
    }

    let chosen_default = default_id.unwrap_or_else(|| {
        choices
            .first()
            .map(|c| c.id.clone())
            .unwrap_or_else(|| LLMId::from("gpt-5.4"))
    });

    let available = AvailableLLMs::new(
        chosen_default.clone(),
        choices,
        Some(chosen_default),
    )
    .expect("Codex model choices are never empty");

    ModelsByFeature {
        agent_mode: available.clone(),
        coding: available.clone(),
        cli_agent: Some(available.clone()),
        computer_use: Some(available),
    }
}

pub fn harnesses_from_snapshot(snap: &CodexSnapshot) -> Vec<HarnessAvailability> {
    let models = snap
        .models
        .iter()
        .map(|m| HarnessModelInfo {
            id: m.slug.clone(),
            display_name: m.display_name.clone(),
            reasoning_level: Some(m.default_effort.clone()),
        })
        .collect();
    vec![HarnessAvailability {
        harness: Harness::Codex,
        display_name: "Codex".to_owned(),
        enabled: true,
        available_models: models,
    }]
}

pub fn chatgpt_connection_from_snapshot(snap: &CodexSnapshot) -> ChatGPTConnectionStatus {
    if snap.logged_in {
        ChatGPTConnectionStatus::Connected(ChatGPTConnection {
            email: Some(
                snap.email
                    .clone()
                    .unwrap_or_else(|| snap.plan_badge()),
            ),
            connected_at: Utc::now().into(),
            token_sharing_active: true,
        })
    } else {
        ChatGPTConnectionStatus::NotConnected
    }
}

pub fn request_usage_from_snapshot(snap: &CodexSnapshot) -> RequestUsageInfo {
    let primary = snap.five_hour.as_ref().or(snap.weekly.as_ref());
    let used_pct = primary
        .map(|w| w.used_percent.round().clamp(0.0, 100.0) as usize)
        .unwrap_or(0);
    let next_refresh = primary
        .and_then(|w| w.resets_at_unix)
        .and_then(|ts| Utc.timestamp_opt(ts, 0).single())
        .unwrap_or_else(|| Utc::now() + chrono::Duration::hours(5));

    RequestUsageInfo {
        request_limit_info: RequestLimitInfo {
            limit: 100,
            num_requests_used_since_refresh: used_pct,
            included_usage_cents: None,
            usage_cents_used_since_last_refresh: None,
            next_refresh_time: ServerTimestamp::new(next_refresh),
            is_unlimited: snap.credits_unlimited && snap.five_hour.is_none(),
            request_limit_refresh_duration: RequestLimitRefreshDuration::Weekly,
            is_unlimited_voice: true,
            voice_request_limit: 10_000,
            voice_requests_used_since_last_refresh: 0,
            is_unlimited_codebase_indices: true,
            max_codebase_indices: 100,
            max_files_per_repo: 50_000,
            embedding_generation_batch_size: 100,
        },
        bonus_grants: Vec::new(),
        workspace_allowances: Vec::new(),
    }
}

pub fn credit_availability_from_snapshot(snap: &CodexSnapshot) -> AICreditAvailability {
    if snap.rate_limit_reached {
        AICreditAvailability::unavailable(AICreditDenialReason::OutOfCredits)
    } else {
        AICreditAvailability::available_with_source(Some(AICreditSource::BaseLimit))
    }
}

// ── Secondary Warp AI Features Backed by Local Codex ─────────────────────────

pub async fn generate_commands(
    prompt: String,
) -> Result<Vec<AIGeneratedCommand>, GenerateCommandsFromNaturalLanguageError> {
    #[derive(Deserialize)]
    struct SchemaOut {
        commands: Vec<SchemaCmd>,
    }
    #[derive(Deserialize)]
    struct SchemaCmd {
        command: String,
        description: String,
        #[serde(default)]
        parameters: Vec<SchemaParam>,
    }
    #[derive(Deserialize)]
    struct SchemaParam {
        id: String,
        description: String,
    }

    let schema = json!({
        "type": "object",
        "properties": {
            "commands": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "description": { "type": "string" },
                        "parameters": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string" },
                                    "description": { "type": "string" }
                                },
                                "required": ["id", "description"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["command", "description", "parameters"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["commands"],
        "additionalProperties": false
    });

    let full_prompt = format!(
        "Generate 1 to 3 macOS/POSIX shell commands matching this request: {prompt}\nUse `{{{{param_name}}}}` placeholders only when user-specific values are required."
    );
    let raw = run_ephemeral_turn(&full_prompt, None, None, Some("low"), Some(schema))
        .await
        .map_err(|_| GenerateCommandsFromNaturalLanguageError::AiProviderError)?;
    let parsed: SchemaOut = serde_json::from_str(&raw)
        .map_err(|_| GenerateCommandsFromNaturalLanguageError::Other)?;

    Ok(parsed
        .commands
        .into_iter()
        .map(|c| {
            AIGeneratedCommand::new(
                c.command,
                c.description,
                c.parameters
                    .into_iter()
                    .map(|p| AIGeneratedCommandParameter::new(p.id, p.description))
                    .collect(),
            )
        })
        .collect())
}

pub async fn generate_dialogue(
    transcript: Vec<TranscriptPart>,
    prompt: String,
) -> anyhow::Result<GenerateDialogueResult> {
    let mut history = String::new();
    for part in transcript {
        history.push_str(&format!(
            "User: {}\nAssistant: {}\n\n",
            part.raw_user_prompt(),
            part.raw_assistant_answer()
        ));
    }
    let full_prompt = if history.is_empty() {
        prompt
    } else {
        format!("Previous conversation:\n{history}User: {prompt}")
    };
    let answer = run_ephemeral_turn(&full_prompt, None, None, Some("low"), None)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let limit_info = request_usage_from_snapshot(&snapshot()).request_limit_info;
    Ok(GenerateDialogueResult::Success {
        answer,
        truncated: false,
        request_limit_info: limit_info,
        transcript_summarized: false,
    })
}

pub async fn generate_workflow_metadata(
    command: String,
) -> Result<GeneratedCommandMetadata, GeneratedCommandMetadataError> {
    #[derive(Deserialize)]
    struct MetaOut {
        command: String,
        title: String,
        description: String,
        arguments: Vec<ArgOut>,
    }
    #[derive(Deserialize)]
    struct ArgOut {
        name: String,
        description: String,
        default_value: String,
    }
    let schema = json!({
        "type": "object",
        "properties": {
            "command": { "type": "string" },
            "title": { "type": "string" },
            "description": { "type": "string" },
            "arguments": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "description": { "type": "string" },
                        "default_value": { "type": "string" }
                    },
                    "required": ["name", "description", "default_value"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["command", "title", "description", "arguments"],
        "additionalProperties": false
    });
    let prompt = format!(
        "Parameterize this shell command for a reusable terminal workflow using `{{{{arg_name}}}}` placeholders and provide a short title, description, and argument descriptions:\n```sh\n{command}\n```"
    );
    let raw = run_ephemeral_turn(&prompt, None, None, Some("low"), Some(schema))
        .await
        .map_err(|_| GeneratedCommandMetadataError::Other)?;
    let out: MetaOut =
        serde_json::from_str(&raw).map_err(|_| GeneratedCommandMetadataError::Other)?;
    Ok(GeneratedCommandMetadata {
        command: out.command,
        title: out.title,
        description: out.description,
        arguments: out
            .arguments
            .into_iter()
            .map(|a| GeneratedArgument {
                name: a.name,
                description: a.description,
                default_value: a.default_value,
            })
            .collect(),
    })
}

pub async fn generate_code_review(
    req: GenerateCodeReviewContentRequest,
) -> anyhow::Result<GenerateCodeReviewContentResponse> {
    let diff_snippet: String = req.diff.chars().take(24_000).collect();
    let task = match req.output_type {
        OutputType::CommitMessage => {
            "Write a clean, concise git commit message (imperative subject line, optional short body, no AI attribution) for this diff:"
        }
        OutputType::PrTitle => "Write a concise pull request title (single line, no quotes) for this diff:",
        OutputType::PrDescription => "Write a clear Markdown pull request summary for this diff:",
    };
    let prompt = format!(
        "{task}\nBranch: {}\nRecent commits: {}\n```diff\n{diff_snippet}\n```",
        req.branch_name,
        req.commit_messages.join("; ")
    );
    let content = run_ephemeral_turn(&prompt, None, None, Some("low"), None)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(GenerateCodeReviewContentResponse { content })
}

pub async fn generate_block_title(
    req: GenerateBlockTitleRequest,
) -> anyhow::Result<GenerateBlockTitleResponse> {
    let out_snippet: String = req.output.chars().take(2_000).collect();
    let prompt = format!(
        "Return ONLY a concise 3-6 word title (no quotes) summarizing this terminal block.\nCommand: {}\nOutput:\n{out_snippet}",
        req.command
    );
    let title = run_ephemeral_turn(&prompt, None, None, Some("low"), None)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(GenerateBlockTitleResponse { title })
}

pub async fn generate_input_suggestions(
    req: &GenerateAIInputSuggestionsRequest,
) -> anyhow::Result<GenerateAIInputSuggestionsResponseV2> {
    #[derive(Deserialize)]
    struct SugOut {
        commands: Vec<String>,
        ai_query: String,
        most_likely_action: String,
    }
    let schema = json!({
        "type": "object",
        "properties": {
            "commands": { "type": "array", "items": { "type": "string" } },
            "ai_query": { "type": "string" },
            "most_likely_action": { "type": "string" }
        },
        "required": ["commands", "ai_query", "most_likely_action"],
        "additionalProperties": false
    });
    let ctx_tail: Vec<&str> = req
        .context_messages
        .iter()
        .rev()
        .take(3)
        .map(String::as_str)
        .collect();
    let prompt = format!(
        "Predict the next shell command (1-3 candidates) based on terminal context.\nPrefix typed so far: {:?}\nRejected: {:?}\nRecent blocks:\n{}\nHistory:\n{}",
        req.prefix,
        req.rejected_suggestions,
        ctx_tail.join("\n---\n"),
        req.history_context.chars().take(1_500).collect::<String>()
    );
    let raw = run_ephemeral_turn(&prompt, None, None, Some("low"), Some(schema))
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let parsed: SugOut = serde_json::from_str(&raw)?;
    let ai_queries = if parsed.ai_query.trim().is_empty() {
        Vec::new()
    } else {
        vec![AgentModeSuggestionV2 {
            query: parsed.ai_query,
            context_block_ids: Vec::new(),
        }]
    };
    Ok(GenerateAIInputSuggestionsResponseV2 {
        commands: parsed.commands,
        ai_queries,
        most_likely_action: parsed.most_likely_action,
    })
}

pub async fn generate_am_suggestions(
    req: &GenerateAMQuerySuggestionsRequest,
) -> anyhow::Result<GenerateAMQuerySuggestionsResponse> {
    let ctx_tail: Vec<&str> = req
        .context_messages
        .iter()
        .rev()
        .take(2)
        .map(String::as_str)
        .collect();
    let prompt = format!(
        "Suggest one short follow-up prompt (single sentence, no quotes) an engineer might ask the coding agent after exit code {}:\n{}",
        req.exit_code,
        ctx_tail.join("\n")
    );
    let text = run_ephemeral_turn(&prompt, None, None, Some("low"), None)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(GenerateAMQuerySuggestionsResponse {
        id: uuid::Uuid::new_v4().to_string(),
        suggestion: (!text.is_empty()).then_some(Suggestion::Simple(SimpleQuery {
            query: text,
            should_plan_task: false,
        })),
    })
}

pub async fn predict_am_query(
    req: &PredictAMQueriesRequest,
) -> anyhow::Result<PredictAMQueriesResponse> {
    let prompt = format!(
        "Complete this partial coding-agent prompt naturally in one concise sentence (return the full completed sentence, no quotes):\nPartial: {}",
        req.partial_query
    );
    let suggestion = run_ephemeral_turn(&prompt, None, None, Some("low"), None)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(PredictAMQueriesResponse { suggestion })
}

pub fn rank_relevant_files_locally(req: &GetRelevantFiles) -> GetRelevantFilesResponse {
    let terms: Vec<String> = req
        .query
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|t| t.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect();

    let mut scored: Vec<(i64, String)> = req
        .files
        .iter()
        .map(|f| {
            let path_lower = f.path.to_ascii_lowercase();
            let sym_lower = f.symbols.to_ascii_lowercase();
            let mut score = 0i64;
            for term in &terms {
                if path_lower.ends_with(term) || path_lower.contains(&format!("/{term}.")) {
                    score += 50;
                } else if path_lower.contains(term) {
                    score += 20;
                }
                if sym_lower.contains(term) {
                    score += 10;
                }
            }
            (score, f.path.clone())
        })
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let relevant_file_paths = scored
        .into_iter()
        .filter(|(s, _)| terms.is_empty() || *s > 0)
        .take(20)
        .map(|(_, p)| p)
        .collect();

    GetRelevantFilesResponse {
        relevant_file_paths,
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
