use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use base64::Engine as _;
use futures::FutureExt;
use prost_types::FieldMask;
use serde_json::{Value, json};
use uuid::Uuid;
use warp_multi_agent_api::client_action::{AddMessagesToTask, AppendToMessageContent, CreateTask};
use warp_multi_agent_api::message::tool_call::apply_file_diffs::{FileDiff, NewFile};
use warp_multi_agent_api::message::tool_call::{
    ApplyFileDiffs, CallMcpTool, RunShellCommand, Tool,
};
use warp_multi_agent_api::message::tool_call_result::Result as ToolResult;
use warp_multi_agent_api::message::update_todos::Operation as TodoOperation;
use warp_multi_agent_api::message::{
    AgentOutput, AgentReasoning, Message as MessageKind, RequestMetadata as ProtoRequestMetadata,
    ToolCall, ToolCallResult, UpdateTodos,
};
use warp_multi_agent_api::response_event::stream_finished::ConversationUsageMetadata;
use warp_multi_agent_api::response_event::{
    ClientActions, StreamFinished, StreamInit, Type as EventType, stream_finished,
};
use warp_multi_agent_api::{
    ApplyFileDiffsResult, CallMcpToolResult, ChargedUsage, ClientAction, CreateTodoList,
    InferenceUsage, MarkTodosCompleted, Message, RequestCharges, ResponseEvent,
    RunShellCommandResult, ShellCommandFinished, Task, TodoItem, TokenCount,
    apply_file_diffs_result, call_mcp_tool_result, client_action, run_shell_command_result,
};

use super::rpc::{CodexClient, get_or_connect};
use super::split_model_and_effort;
use crate::ai::agent::api::{RequestParams, ResponseStream, serde_json_to_prost};
use crate::ai::agent::{AIAgentAttachment, AIAgentContext, AIAgentInput, UserQueryMode};
use crate::server::server_api::AIApiError;

const AGENT_OUTPUT_TEXT_PATH: &str = "agent_output.text";
const AGENT_REASONING_TEXT_PATH: &str = "agent_reasoning.reasoning";

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveTextKind {
    Output,
    Reasoning,
}

struct OpenTextState {
    kind: ActiveTextKind,
    codex_item_id: String,
    warp_message_id: String,
}

struct TrackedItem {
    warp_action_id: String,
    command: Option<String>,
}

pub async fn generate_codex_multi_agent_output(
    params: RequestParams,
    cancellation_rx: futures::channel::oneshot::Receiver<()>,
) -> ResponseStream {
    let (tx, rx) = async_channel::unbounded::<Result<ResponseEvent, Arc<AIApiError>>>();

    std::thread::Builder::new()
        .name("warp-codex-turn".to_owned())
        .spawn(move || {
            futures::executor::block_on(drive_codex_turn(params, cancellation_rx, tx));
        })
        .ok();

    Box::pin(rx)
}

async fn drive_codex_turn(
    params: RequestParams,
    cancellation_rx: futures::channel::oneshot::Receiver<()>,
    tx: async_channel::Sender<Result<ResponseEvent, Arc<AIApiError>>>,
) {
    let client = match get_or_connect().await {
        Ok(c) => c,
        Err(err) => {
            let _ = tx
                .send(Err(Arc::new(AIApiError::Other(anyhow::anyhow!("{err}")))))
                .await;
            return;
        }
    };

    let is_new_conversation = params.conversation_token.is_none();
    let existing_tid = params
        .conversation_token
        .as_ref()
        .map(|t| t.as_str().to_owned())
        .filter(|s| !s.is_empty());

    let root_task_id = params
        .tasks
        .first()
        .map(|t| t.id.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let (prompt_text, image_paths, cwd, query_mode, is_passive) = extract_turn_inputs(&params);
    let (selected_model, selected_effort) = split_model_and_effort(params.model.as_str());

    let sandbox_mode = match params.isolation_level {
        warp_multi_agent_api::IsolationLevel::Sandbox => "workspace-write",
        _ => "danger-full-access",
    };

    let mut notif_rx = client.subscribe();

    // 1. Start or resume Codex thread
    let (thread_id, resolved_model) = match setup_thread(
        &client,
        existing_tid.as_deref(),
        &cwd,
        selected_model.as_deref(),
        sandbox_mode,
        is_passive,
    )
    .await
    {
        Ok(res) => res,
        Err(err) => {
            let friendly = format_codex_turn_error(&err);
            let _ = tx
                .send(Err(Arc::new(AIApiError::Other(anyhow::anyhow!(
                    "{friendly}"
                )))))
                .await;
            return;
        }
    };

    let request_id = Uuid::new_v4().to_string();

    // 2. Build turn/start input items (text + any attached local images)
    let mut input_items = Vec::new();
    if !prompt_text.trim().is_empty() || image_paths.is_empty() {
        input_items.push(json!({
            "type": "text",
            "text": if prompt_text.trim().is_empty() { "Continue." } else { &prompt_text },
            "text_elements": [],
        }));
    }
    for img in image_paths {
        input_items.push(json!({
            "type": "localImage",
            "path": img,
        }));
    }

    let mut turn_params = json!({
        "threadId": thread_id,
        "cwd": cwd,
        "approvalPolicy": "never",
        "input": input_items,
    });
    if let Some(ref m) = selected_model {
        turn_params["model"] = Value::String(m.clone());
    }
    if let Some(ref eff) = selected_effort {
        turn_params["effort"] = Value::String(eff.clone());
    }
    if query_mode == UserQueryMode::Plan {
        let model_for_plan = selected_model
            .clone()
            .unwrap_or_else(|| resolved_model.clone());
        turn_params["collaborationMode"] = json!({
            "mode": "plan",
            "settings": {
                "model": model_for_plan,
                "reasoning_effort": selected_effort,
                "developer_instructions": null,
            }
        });
    }

    let turn_res = match client.call("turn/start", turn_params).await {
        Ok(r) => r,
        Err(err) => {
            let friendly = format_codex_turn_error(&err);
            let _ = tx
                .send(Err(Arc::new(AIApiError::Other(anyhow::anyhow!(
                    "{friendly}"
                )))))
                .await;
            return;
        }
    };
    let turn_id = turn_res["turn"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();

    // 3. Emit StreamInit + initial CreateTask (on first turn of conversation)
    let init = StreamInit {
        conversation_id: thread_id.clone(),
        request_id: request_id.clone(),
        run_id: turn_id.clone(),
    };
    if tx
        .send(Ok(ResponseEvent {
            r#type: Some(EventType::Init(init)),
        }))
        .await
        .is_err()
    {
        let _ = interrupt_turn(&client, &thread_id, &turn_id).await;
        return;
    }

    if is_new_conversation {
        let create = ClientAction {
            action: Some(client_action::Action::CreateTask(CreateTask {
                task: Some(Task {
                    id: root_task_id.clone(),
                    ..Default::default()
                }),
            })),
        };
        let _ = send_actions(&tx, vec![create]).await;
    }

    // 4. Stream notifications until turn/completed or cancellation
    let mut cancel_fused = cancellation_rx.fuse();
    let mut open_text: Option<OpenTextState> = None;
    let mut tracked_items: HashMap<String, TrackedItem> = HashMap::new();
    let mut todo_list_id: Option<String> = None;
    let mut last_in_tokens: u64 = 0;
    let mut last_cached_tokens: u64 = 0;
    let mut last_out_tokens: u64 = 0;
    let mut context_window: u64 = 200_000;
    let mut turn_error: Option<String> = None;

    loop {
        futures::select! {
            _ = cancel_fused => {
                let _ = interrupt_turn(&client, &thread_id, &turn_id).await;
                return;
            }
            maybe_notif = notif_rx.recv().fuse() => {
                let Ok(notif) = maybe_notif else {
                    turn_error = Some("Codex app-server disconnected during turn".to_owned());
                    break;
                };
                let p = &notif.params;
                let notif_tid = p["threadId"].as_str().unwrap_or_default();
                if !notif_tid.is_empty() && notif_tid != thread_id {
                    continue;
                }

                match notif.method.as_str() {
                    "item/agentMessage/delta" => {
                        let item_id = p["itemId"].as_str().unwrap_or("msg").to_owned();
                        if let Some(delta) = p["delta"].as_str()
                            && !delta.is_empty()
                        {
                            let actions = append_text_chunk(
                                &mut open_text,
                                ActiveTextKind::Output,
                                &item_id,
                                delta,
                                &root_task_id,
                                &request_id,
                            );
                            if send_actions(&tx, actions).await.is_err() {
                                let _ = interrupt_turn(&client, &thread_id, &turn_id).await;
                                return;
                            }
                        }
                    }
                    "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                        let item_id = p["itemId"].as_str().unwrap_or("reasoning").to_owned();
                        if let Some(delta) = p["delta"].as_str()
                            && !delta.is_empty()
                        {
                            let actions = append_text_chunk(
                                &mut open_text,
                                ActiveTextKind::Reasoning,
                                &item_id,
                                delta,
                                &root_task_id,
                                &request_id,
                            );
                            if send_actions(&tx, actions).await.is_err() {
                                let _ = interrupt_turn(&client, &thread_id, &turn_id).await;
                                return;
                            }
                        }
                    }
                    "turn/plan/updated" => {
                        if let Some(plan_arr) = p["plan"].as_array() {
                            open_text = None;
                            let actions = map_plan_update(
                                &mut todo_list_id,
                                plan_arr,
                                &root_task_id,
                                &request_id,
                            );
                            let _ = send_actions(&tx, actions).await;
                        }
                    }
                    "item/started" => {
                        let item = &p["item"];
                        let actions = map_item_started(
                            item,
                            &mut open_text,
                            &mut tracked_items,
                            &root_task_id,
                            &request_id,
                        );
                        if !actions.is_empty() && send_actions(&tx, actions).await.is_err() {
                            let _ = interrupt_turn(&client, &thread_id, &turn_id).await;
                            return;
                        }
                    }
                    "item/completed" => {
                        let item = &p["item"];
                        let actions = map_item_completed(
                            item,
                            &mut open_text,
                            &mut tracked_items,
                            &root_task_id,
                            &request_id,
                        );
                        if !actions.is_empty() && send_actions(&tx, actions).await.is_err() {
                            let _ = interrupt_turn(&client, &thread_id, &turn_id).await;
                            return;
                        }
                    }
                    "thread/tokenUsage/updated" => {
                        let usage = &p["tokenUsage"];
                        let last = &usage["last"];
                        last_in_tokens = last["inputTokens"].as_u64().unwrap_or(last_in_tokens);
                        last_cached_tokens = last["cachedInputTokens"]
                            .as_u64()
                            .unwrap_or(last_cached_tokens)
                            .min(last_in_tokens);
                        last_out_tokens = last["outputTokens"].as_u64().unwrap_or(last_out_tokens);
                        if let Some(win) = usage["modelContextWindow"].as_u64().filter(|&w| w > 0) {
                            context_window = win;
                        }
                    }
                    "error" => {
                        let headline = p["error"]["message"].as_str().unwrap_or_default();
                        if !headline.is_empty() && !headline.starts_with("Reconnecting...") {
                            turn_error = Some(headline.to_owned());
                        }
                    }
                    "turn/completed" => {
                        let t = &p["turn"];
                        if !turn_id.is_empty()
                            && let Some(done_id) = t["id"].as_str()
                            && done_id != turn_id
                        {
                            continue;
                        }
                        if t["status"].as_str() == Some("failed") {
                            let msg = t["error"]["message"]
                                .as_str()
                                .or(turn_error.as_deref())
                                .unwrap_or("Codex turn failed");
                            turn_error = Some(msg.to_owned());
                        } else {
                            turn_error = None;
                        }
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    if let Some(err) = turn_error {
        let friendly = format_codex_turn_error(&err);
        let _ = tx
            .send(Err(Arc::new(AIApiError::Other(anyhow::anyhow!(
                "{friendly}"
            )))))
            .await;
        return;
    }

    // 5. Emit RequestMetadata + StreamFinished with live Codex token & context-window stats
    let model_label = selected_model.unwrap_or(resolved_model);
    let uncached_in = last_in_tokens.saturating_sub(last_cached_tokens);
    if last_in_tokens > 0 || last_out_tokens > 0 {
        let mut by_model = HashMap::new();
        by_model.insert(
            model_label.clone(),
            InferenceUsage {
                token_count: Some(TokenCount {
                    input: uncached_in,
                    output: last_out_tokens,
                    input_cache_read: last_cached_tokens,
                    input_cache_write: 0,
                }),
                ..Default::default()
            },
        );
        let mut usage_by_category = HashMap::new();
        usage_by_category.insert(
            "primary_agent".to_owned(),
            ChargedUsage {
                direct_api_inference_usage: by_model,
                ..Default::default()
            },
        );
        let meta_msg = make_message(
            &root_task_id,
            &request_id,
            MessageKind::RequestMetadata(ProtoRequestMetadata {
                charges: Some(RequestCharges {
                    usage_by_category,
                    ..Default::default()
                }),
                ..Default::default()
            }),
        );
        let _ = send_actions(
            &tx,
            vec![add_messages_action(&root_task_id, vec![meta_msg])],
        )
        .await;
    }

    let ctx_fraction = if context_window > 0 {
        (last_in_tokens as f32 / context_window as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    #[allow(deprecated)]
    let finished = StreamFinished {
        reason: Some(stream_finished::Reason::Done(stream_finished::Done {})),
        conversation_usage_metadata: Some(ConversationUsageMetadata {
            context_window_usage: ctx_fraction,
            total_input_tokens: u32::try_from(last_in_tokens).unwrap_or(u32::MAX),
            summarized: false,
            credits_spent: 0.0,
            ..Default::default()
        }),
        token_usage: Vec::new(),
        should_refresh_model_config: false,
        request_cost: None,
        request_charges: None,
    };
    let _ = tx
        .send(Ok(ResponseEvent {
            r#type: Some(EventType::Finished(finished)),
        }))
        .await;
}

async fn setup_thread(
    client: &CodexClient,
    existing_tid: Option<&str>,
    cwd: &str,
    selected_model: Option<&str>,
    sandbox_mode: &str,
    is_passive: bool,
) -> Result<(String, String), String> {
    if let Some(tid) = existing_tid
        && !is_passive
    {
        let mut resume_params = json!({
            "threadId": tid,
            "cwd": cwd,
            "approvalPolicy": "never",
            "sandbox": sandbox_mode,
            "excludeTurns": true,
        });
        if let Some(m) = selected_model {
            resume_params["model"] = Value::String(m.to_owned());
        }
        if let Ok(res) = client.call("thread/resume", resume_params).await {
            let model = res["model"].as_str().unwrap_or("gpt-5.4").to_owned();
            return Ok((tid.to_owned(), model));
        }
    }

    let mut start_params = json!({
        "cwd": cwd,
        "approvalPolicy": "never",
        "sandbox": if is_passive { "read-only" } else { sandbox_mode },
        "ephemeral": is_passive,
    });
    if let Some(m) = selected_model {
        start_params["model"] = Value::String(m.to_owned());
    }
    let res = client.call("thread/start", start_params).await?;
    let tid = res["thread"]["id"]
        .as_str()
        .ok_or_else(|| "Codex thread/start returned no thread ID".to_owned())?
        .to_owned();
    let model = res["model"].as_str().unwrap_or("gpt-5.4").to_owned();
    Ok((tid, model))
}

async fn interrupt_turn(
    client: &CodexClient,
    thread_id: &str,
    turn_id: &str,
) -> Result<(), String> {
    if thread_id.is_empty() || turn_id.is_empty() {
        return Ok(());
    }
    client
        .call(
            "turn/interrupt",
            json!({
                "threadId": thread_id,
                "turnId": turn_id,
            }),
        )
        .await
        .map(|_| ())
}

fn format_codex_turn_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("unauthorized")
        || lower.contains("not logged in")
        || lower.contains("authentication")
        || lower.contains("login required")
    {
        "Codex CLI is not signed in. Open Settings -> Billing and Usage (or run `codex login`) to sign in with your ChatGPT plan.".to_owned()
    } else {
        raw.to_owned()
    }
}

fn extract_turn_inputs(
    params: &RequestParams,
) -> (String, Vec<String>, String, UserQueryMode, bool) {
    let mut prompt_sections = Vec::new();
    let mut images = Vec::new();
    let mut cwd: Option<PathBuf> = None;
    let mut query_mode = UserQueryMode::Normal;
    let mut is_passive = false;

    for input in &params.input {
        if let Some(ctx_slice) = input.context() {
            for ctx_item in ctx_slice {
                match ctx_item {
                    AIAgentContext::Directory { pwd: Some(dir), .. } if cwd.is_none() => {
                        cwd = Some(PathBuf::from(dir));
                    }
                    AIAgentContext::SelectedText(text) if !text.trim().is_empty() => {
                        prompt_sections.push(format!("Selected text:\n```\n{text}\n```"));
                    }
                    AIAgentContext::Image(img) => {
                        if let Ok(bytes) =
                            base64::engine::general_purpose::STANDARD.decode(&img.data)
                        {
                            let ext = if img.mime_type.contains("jpeg")
                                || img.mime_type.contains("jpg")
                            {
                                "jpg"
                            } else if img.mime_type.contains("webp") {
                                "webp"
                            } else {
                                "png"
                            };
                            let path = std::env::temp_dir()
                                .join(format!("warp-codex-img-{}.{ext}", Uuid::new_v4()));
                            if std::fs::write(&path, bytes).is_ok() {
                                images.push(path.to_string_lossy().into_owned());
                            }
                        }
                    }
                    AIAgentContext::Block(block) if !block.is_auto_attached => {
                        let snippet: String = block.output.chars().take(8_000).collect();
                        prompt_sections.push(format!(
                            "Terminal block (`{}`, exit {:?}):\n```\n{snippet}\n```",
                            block.command, block.exit_code
                        ));
                    }
                    AIAgentContext::File(file) => {
                        prompt_sections.push(format!("Referenced file context: {file:?}"));
                    }
                    _ => {}
                }
            }
        }
        match input {
            AIAgentInput::UserQuery {
                query,
                referenced_attachments,
                user_query_mode,
                running_command,
                ..
            } => {
                query_mode = *user_query_mode;
                for att in referenced_attachments.values() {
                    append_attachment_context(att, &mut prompt_sections, &mut images);
                }
                if let Some(rc) = running_command {
                    let snap: String = rc.grid_contents.chars().take(6_000).collect();
                    prompt_sections.push(format!(
                        "Running command (`{}`):\n```\n{snap}\n```",
                        rc.command
                    ));
                }
                if !query.trim().is_empty() {
                    prompt_sections.push(query.clone());
                }
            }
            AIAgentInput::AutoCodeDiffQuery { query, .. }
            | AIAgentInput::CreateNewProject { query, .. } => {
                prompt_sections.push(query.clone());
            }
            AIAgentInput::InitProjectRules { .. } => {
                prompt_sections.push(
                    "Inspect this repository and create or update AGENTS.md with concise build, test, and architecture guidance.".to_owned(),
                );
            }
            AIAgentInput::CloneRepository { clone_repo_url, .. } => {
                prompt_sections.push(format!(
                    "Clone repository {} and help set it up locally.",
                    clone_repo_url.clone().into_url()
                ));
            }
            AIAgentInput::SummarizeConversation { prompt, .. } => {
                prompt_sections.push(
                    prompt
                        .clone()
                        .unwrap_or_else(|| "Summarize this conversation concisely.".to_owned()),
                );
            }
            AIAgentInput::InvokeSkill {
                skill, user_query, ..
            } => {
                let q = user_query
                    .as_ref()
                    .map(|u| u.query.as_str())
                    .unwrap_or_default();
                prompt_sections.push(format!("Run skill `{}`.\n{q}", skill.name));
            }
            AIAgentInput::CodeReview {
                review_comments, ..
            } => {
                let mut text = String::from("Address the following code review comments:\n");
                for c in &review_comments.comments {
                    text.push_str(&format!("- {:?}: {}\n", c.target, c.content));
                }
                prompt_sections.push(text);
            }
            AIAgentInput::TriggerPassiveSuggestion {
                attachments,
                trigger,
                ..
            } => {
                is_passive = true;
                for att in attachments {
                    append_attachment_context(att, &mut prompt_sections, &mut images);
                }
                prompt_sections.push(format!(
                    "Briefly suggest the single most helpful fix or next shell command for this terminal event ({trigger:?})."
                ));
            }
            _ => {}
        }
    }

    let resolved_cwd = cwd
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .to_string_lossy()
        .into_owned();

    (
        prompt_sections.join("\n\n"),
        images,
        resolved_cwd,
        query_mode,
        is_passive,
    )
}

fn append_attachment_context(
    att: &AIAgentAttachment,
    sections: &mut Vec<String>,
    _images: &mut Vec<String>,
) {
    match att {
        AIAgentAttachment::FilePathReference { file_path, .. } => {
            sections.push(format!("Attached file: `{file_path}`"));
        }
        AIAgentAttachment::PlainText(text) => {
            sections.push(text.clone());
        }
        AIAgentAttachment::DocumentContent { content, .. } => {
            sections.push(content.clone());
        }
        AIAgentAttachment::DiffHunk {
            file_path,
            diff_content,
            ..
        } => {
            sections.push(format!(
                "Diff hunk in `{file_path}`:\n```diff\n{diff_content}\n```"
            ));
        }
        AIAgentAttachment::DiffSet { file_diffs, .. } => {
            for (path, hunks) in file_diffs {
                for h in hunks {
                    sections.push(format!(
                        "Diff in `{path}`:\n```diff\n{}\n```",
                        h.diff_content
                    ));
                }
            }
        }
        AIAgentAttachment::Block(block) => {
            let snippet: String = block.output.chars().take(8_000).collect();
            sections.push(format!(
                "Terminal block (`{}`, exit {:?}):\n```\n{snippet}\n```",
                block.command, block.exit_code
            ));
        }
        _ => {}
    }
}

fn append_text_chunk(
    open_text: &mut Option<OpenTextState>,
    kind: ActiveTextKind,
    codex_item_id: &str,
    delta: &str,
    task_id: &str,
    request_id: &str,
) -> Vec<ClientAction> {
    let continues = open_text
        .as_ref()
        .is_some_and(|s| s.kind == kind && s.codex_item_id == codex_item_id);

    let payload = match kind {
        ActiveTextKind::Output => MessageKind::AgentOutput(AgentOutput {
            text: delta.to_owned(),
        }),
        ActiveTextKind::Reasoning => MessageKind::AgentReasoning(AgentReasoning {
            reasoning: delta.to_owned(),
            finished_duration: None,
        }),
    };

    if continues {
        let warp_msg_id = open_text
            .as_ref()
            .map(|s| s.warp_message_id.clone())
            .unwrap_or_default();
        let mask_path = match kind {
            ActiveTextKind::Output => AGENT_OUTPUT_TEXT_PATH,
            ActiveTextKind::Reasoning => AGENT_REASONING_TEXT_PATH,
        };
        return vec![ClientAction {
            action: Some(client_action::Action::AppendToMessageContent(
                AppendToMessageContent {
                    task_id: task_id.to_owned(),
                    message: Some(Message {
                        id: warp_msg_id,
                        task_id: task_id.to_owned(),
                        request_id: request_id.to_owned(),
                        message: Some(payload),
                        ..Default::default()
                    }),
                    mask: Some(FieldMask {
                        paths: vec![mask_path.to_owned()],
                    }),
                },
            )),
        }];
    }

    let msg = make_message(task_id, request_id, payload);
    *open_text = Some(OpenTextState {
        kind,
        codex_item_id: codex_item_id.to_owned(),
        warp_message_id: msg.id.clone(),
    });
    vec![add_messages_action(task_id, vec![msg])]
}

fn map_item_started(
    item: &Value,
    open_text: &mut Option<OpenTextState>,
    tracked: &mut HashMap<String, TrackedItem>,
    task_id: &str,
    request_id: &str,
) -> Vec<ClientAction> {
    let item_type = item["type"].as_str().unwrap_or_default();
    let codex_id = item["id"].as_str().unwrap_or_default().to_owned();
    if codex_id.is_empty() {
        return Vec::new();
    }

    match item_type {
        "commandExecution" => {
            *open_text = None;
            let cmd = item["command"].as_str().unwrap_or_default().to_owned();
            let action_id = Uuid::new_v4().to_string();
            tracked.insert(
                codex_id,
                TrackedItem {
                    warp_action_id: action_id.clone(),
                    command: Some(cmd.clone()),
                },
            );
            let msg = make_message(
                task_id,
                request_id,
                MessageKind::ToolCall(ToolCall {
                    tool_call_id: action_id,
                    tool: Some(Tool::RunShellCommand(RunShellCommand {
                        command: cmd,
                        ..Default::default()
                    })),
                }),
            );
            vec![add_messages_action(task_id, vec![msg])]
        }
        "mcpToolCall" => {
            *open_text = None;
            let name = item["tool"].as_str().unwrap_or("mcp_tool").to_owned();
            let server_id = item["server"].as_str().unwrap_or("mcp").to_owned();
            let args = item
                .get("arguments")
                .cloned()
                .and_then(|v| serde_json_to_prost(v).ok())
                .and_then(|v| match v.kind {
                    Some(prost_types::value::Kind::StructValue(s)) => Some(s),
                    _ => None,
                })
                .unwrap_or_default();
            let action_id = Uuid::new_v4().to_string();
            tracked.insert(
                codex_id,
                TrackedItem {
                    warp_action_id: action_id.clone(),
                    command: None,
                },
            );
            let msg = make_message(
                task_id,
                request_id,
                MessageKind::ToolCall(ToolCall {
                    tool_call_id: action_id,
                    tool: Some(Tool::CallMcpTool(CallMcpTool {
                        name,
                        args: Some(args),
                        server_id,
                        server_identity: None,
                    })),
                }),
            );
            vec![add_messages_action(task_id, vec![msg])]
        }
        "webSearch" => {
            *open_text = None;
            let query = item["query"].as_str().unwrap_or_default();
            let args = match serde_json_to_prost(json!({ "query": query }))
                .ok()
                .and_then(|v| v.kind)
            {
                Some(prost_types::value::Kind::StructValue(s)) => s,
                _ => prost_types::Struct::default(),
            };
            let action_id = Uuid::new_v4().to_string();
            tracked.insert(
                codex_id,
                TrackedItem {
                    warp_action_id: action_id.clone(),
                    command: None,
                },
            );
            let msg = make_message(
                task_id,
                request_id,
                MessageKind::ToolCall(ToolCall {
                    tool_call_id: action_id,
                    tool: Some(Tool::CallMcpTool(CallMcpTool {
                        name: "web_search".to_owned(),
                        args: Some(args),
                        server_id: "codex".to_owned(),
                        server_identity: None,
                    })),
                }),
            );
            vec![add_messages_action(task_id, vec![msg])]
        }
        _ => Vec::new(),
    }
}

fn map_item_completed(
    item: &Value,
    open_text: &mut Option<OpenTextState>,
    tracked: &mut HashMap<String, TrackedItem>,
    task_id: &str,
    request_id: &str,
) -> Vec<ClientAction> {
    let item_type = item["type"].as_str().unwrap_or_default();
    let codex_id = item["id"].as_str().unwrap_or_default();

    match item_type {
        "agentMessage" => {
            if let Some(text) = item["text"].as_str()
                && !text.is_empty()
                && open_text
                    .as_ref()
                    .is_none_or(|s| s.codex_item_id != codex_id)
            {
                *open_text = None;
                let msg = make_message(
                    task_id,
                    request_id,
                    MessageKind::AgentOutput(AgentOutput {
                        text: text.to_owned(),
                    }),
                );
                return vec![add_messages_action(task_id, vec![msg])];
            }
            *open_text = None;
            Vec::new()
        }
        "commandExecution" => {
            let entry = tracked.remove(codex_id);
            let action_id = entry
                .as_ref()
                .map(|e| e.warp_action_id.clone())
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            let cmd = entry
                .and_then(|e| e.command)
                .or_else(|| item["command"].as_str().map(str::to_owned))
                .unwrap_or_default();
            let output = item["aggregatedOutput"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let exit_code = item["exitCode"]
                .as_i64()
                .map(|c| c as i32)
                .unwrap_or_else(|| i32::from(item["status"].as_str() == Some("failed")));
            #[allow(deprecated)]
            let res_msg = make_message(
                task_id,
                request_id,
                MessageKind::ToolCallResult(ToolCallResult {
                    tool_call_id: action_id.clone(),
                    context: None,
                    result: Some(ToolResult::RunShellCommand(RunShellCommandResult {
                        command: cmd,
                        output: output.clone(),
                        exit_code,
                        result: Some(run_shell_command_result::Result::CommandFinished(
                            ShellCommandFinished {
                                output,
                                exit_code,
                                command_id: action_id,
                                start_ts: None,
                                finish_ts: None,
                            },
                        )),
                    })),
                }),
            );
            vec![add_messages_action(task_id, vec![res_msg])]
        }
        "fileChange" => {
            *open_text = None;
            let action_id = Uuid::new_v4().to_string();
            let mut diffs = ApplyFileDiffs {
                summary: "Edited files via Codex".to_owned(),
                ..Default::default()
            };
            if let Some(changes) = item["changes"].as_array() {
                for ch in changes {
                    let path = ch["path"].as_str().unwrap_or_default().to_owned();
                    let diff_text = ch["diff"].as_str().unwrap_or_default().to_owned();
                    let kind = ch["kind"]["type"].as_str().unwrap_or("update");
                    if kind == "add" {
                        diffs.new_files.push(NewFile {
                            file_path: path,
                            content: diff_text,
                            allow_overwrite: true,
                        });
                    } else {
                        diffs.diffs.push(FileDiff {
                            file_path: path,
                            search: String::new(),
                            replace: diff_text,
                        });
                    }
                }
            }
            let call_msg = make_message(
                task_id,
                request_id,
                MessageKind::ToolCall(ToolCall {
                    tool_call_id: action_id.clone(),
                    tool: Some(Tool::ApplyFileDiffs(diffs)),
                }),
            );
            let failed = item["status"].as_str() == Some("failed");
            let result_inner = if failed {
                apply_file_diffs_result::Result::Error(apply_file_diffs_result::Error {
                    message: "File patch failed".to_owned(),
                })
            } else {
                apply_file_diffs_result::Result::Success(apply_file_diffs_result::Success {
                    ..Default::default()
                })
            };
            let res_msg = make_message(
                task_id,
                request_id,
                MessageKind::ToolCallResult(ToolCallResult {
                    tool_call_id: action_id,
                    context: None,
                    result: Some(ToolResult::ApplyFileDiffs(ApplyFileDiffsResult {
                        result: Some(result_inner),
                    })),
                }),
            );
            vec![add_messages_action(task_id, vec![call_msg, res_msg])]
        }
        "mcpToolCall" | "webSearch" => {
            let Some(entry) = tracked.remove(codex_id) else {
                return Vec::new();
            };
            let failed = item["status"].as_str() == Some("failed");
            let text = item
                .get("result")
                .or_else(|| item.get("error"))
                .map(Value::to_string)
                .unwrap_or_else(|| "Completed".to_owned());
            let mcp_res = if failed {
                call_mcp_tool_result::Result::Error(call_mcp_tool_result::Error { message: text })
            } else {
                call_mcp_tool_result::Result::Success(call_mcp_tool_result::Success {
                    results: vec![call_mcp_tool_result::success::Result {
                        result: Some(call_mcp_tool_result::success::result::Result::Text(
                            call_mcp_tool_result::success::result::Text { text },
                        )),
                    }],
                })
            };
            let res_msg = make_message(
                task_id,
                request_id,
                MessageKind::ToolCallResult(ToolCallResult {
                    tool_call_id: entry.warp_action_id,
                    context: None,
                    result: Some(ToolResult::CallMcpTool(CallMcpToolResult {
                        result: Some(mcp_res),
                    })),
                }),
            );
            vec![add_messages_action(task_id, vec![res_msg])]
        }
        _ => Vec::new(),
    }
}

fn map_plan_update(
    todo_list_id: &mut Option<String>,
    plan_arr: &[Value],
    task_id: &str,
    request_id: &str,
) -> Vec<ClientAction> {
    *todo_list_id = Some(Uuid::new_v4().to_string());
    let mut items = Vec::new();
    let mut completed_ids = Vec::new();

    for (idx, step) in plan_arr.iter().enumerate() {
        let title = step["step"].as_str().unwrap_or_default().to_owned();
        if title.is_empty() {
            continue;
        }
        let id = format!("step-{idx}");
        if step["status"].as_str() == Some("completed") {
            completed_ids.push(id.clone());
        }
        items.push(TodoItem {
            id,
            title,
            description: String::new(),
        });
    }
    if items.is_empty() {
        return Vec::new();
    }

    let mut msgs = vec![make_message(
        task_id,
        request_id,
        MessageKind::UpdateTodos(UpdateTodos {
            operation: Some(TodoOperation::CreateTodoList(CreateTodoList {
                initial_todos: items,
            })),
        }),
    )];
    if !completed_ids.is_empty() {
        msgs.push(make_message(
            task_id,
            request_id,
            MessageKind::UpdateTodos(UpdateTodos {
                operation: Some(TodoOperation::MarkTodosCompleted(MarkTodosCompleted {
                    todo_ids: completed_ids,
                })),
            }),
        ));
    }
    vec![add_messages_action(task_id, msgs)]
}

fn make_message(task_id: &str, request_id: &str, kind: MessageKind) -> Message {
    Message {
        id: Uuid::new_v4().to_string(),
        task_id: task_id.to_owned(),
        request_id: request_id.to_owned(),
        timestamp: Some(prost_types::Timestamp::from(SystemTime::now())),
        message: Some(kind),
        ..Default::default()
    }
}

fn add_messages_action(task_id: &str, messages: Vec<Message>) -> ClientAction {
    ClientAction {
        action: Some(client_action::Action::AddMessagesToTask(
            AddMessagesToTask {
                task_id: task_id.to_owned(),
                messages,
            },
        )),
    }
}

async fn send_actions(
    tx: &async_channel::Sender<Result<ResponseEvent, Arc<AIApiError>>>,
    actions: Vec<ClientAction>,
) -> Result<(), ()> {
    if actions.is_empty() {
        return Ok(());
    }
    tx.send(Ok(ResponseEvent {
        r#type: Some(EventType::ClientActions(ClientActions { actions })),
    }))
    .await
    .map_err(|_| ())
}
