use serde_json::json;

use super::rpc::{
    CodexSnapshot, CodexWindowQuota, merge_rate_limits_into_snapshot, parse_model_entry,
    snapshots_meaningfully_equal,
};
use super::ui::{format_quota_reset_label, format_reset_time};
use super::{
    credit_availability_from_snapshot, models_by_feature_from_snapshot,
    rank_relevant_files_locally, request_usage_from_snapshot, split_model_and_effort,
};
use crate::ai::get_relevant_files::api::{FileContext, GetRelevantFiles};

#[test]
fn parses_codex_model_entry_and_efforts() {
    let raw = json!({
        "id": "gpt-5.4",
        "model": "gpt-5.4",
        "displayName": "GPT-5.4",
        "description": "Frontier coding model",
        "hidden": false,
        "defaultReasoningEffort": "medium",
        "supportedReasoningEfforts": [
            { "reasoningEffort": "low", "description": "Fast" },
            { "reasoningEffort": "medium", "description": "Balanced" },
            { "reasoningEffort": "high", "description": "Deep" }
        ],
        "inputModalities": ["text", "image"],
        "isDefault": true
    });
    let entry = parse_model_entry(&raw).expect("valid model");
    assert_eq!(entry.slug, "gpt-5.4");
    assert_eq!(entry.display_name, "GPT-5.4");
    assert_eq!(entry.default_effort, "medium");
    assert_eq!(entry.supported_efforts.len(), 3);
    assert!(entry.vision_supported);
    assert!(entry.is_default);

    let snap = CodexSnapshot {
        logged_in: true,
        plan_type: Some("pro".to_owned()),
        models: vec![entry],
        ..CodexSnapshot::default()
    };
    assert_eq!(snap.plan_badge(), "ChatGPT Pro");

    let by_feature = models_by_feature_from_snapshot(&snap);
    let ids: Vec<String> = by_feature
        .agent_mode
        .info_for_id(&"gpt-5.4".into())
        .into_iter()
        .chain(by_feature.agent_mode.info_for_id(&"gpt-5.4::high".into()))
        .map(|m| m.id.to_string())
        .collect();
    assert_eq!(ids, vec!["gpt-5.4", "gpt-5.4::high"]);
}

#[test]
fn splits_model_and_reasoning_effort_variants() {
    assert_eq!(split_model_and_effort("auto"), (None, None));
    assert_eq!(
        split_model_and_effort("gpt-5.4"),
        (Some("gpt-5.4".to_owned()), None)
    );
    assert_eq!(
        split_model_and_effort("gpt-5.4::xhigh"),
        (Some("gpt-5.4".to_owned()), Some("xhigh".to_owned()))
    );
}

#[test]
fn merges_codex_rate_limits_and_computes_availability() {
    let mut snap = CodexSnapshot {
        logged_in: true,
        ..CodexSnapshot::default()
    };
    let rl = json!({
        "planType": "plus",
        "primary": {
            "usedPercent": 42.5,
            "windowDurationMins": 300,
            "resetsAt": 1_800_000_000
        },
        "secondary": {
            "usedPercent": 15.0,
            "windowDurationMins": 10080,
            "resetsAt": 1_800_500_000
        },
        "credits": {
            "hasCredits": true,
            "unlimited": false,
            "balance": "$18.50"
        },
        "rateLimitReachedType": null
    });
    merge_rate_limits_into_snapshot(&mut snap, &rl);
    assert_eq!(snap.plan_badge(), "ChatGPT Plus");
    assert_eq!(snap.five_hour.as_ref().unwrap().used_percent, 42.5);
    assert_eq!(snap.weekly.as_ref().unwrap().used_percent, 15.0);
    assert_eq!(snap.credits_balance.as_deref(), Some("$18.50"));
    assert!(!snap.rate_limit_reached);
    assert!(credit_availability_from_snapshot(&snap).available);

    let usage = request_usage_from_snapshot(&snap);
    assert_eq!(usage.request_limit_info.limit, 100);
    assert_eq!(usage.request_limit_info.num_requests_used_since_refresh, 43);

    let exhausted = json!({
        "primary": { "usedPercent": 100.0, "windowDurationMins": 300, "resetsAt": 1_800_000_000 },
        "rateLimitReachedType": "rate_limit_reached"
    });
    merge_rate_limits_into_snapshot(&mut snap, &exhausted);
    assert!(snap.rate_limit_reached);
    assert!(!credit_availability_from_snapshot(&snap).available);
}

#[test]
fn ranks_relevant_files_by_path_and_symbol_hits() {
    let req = GetRelevantFiles {
        query: "codex rate limits".to_owned(),
        files: vec![
            FileContext {
                path: "src/terminal/pty.rs".to_owned(),
                symbols: "spawn_pty resize".to_owned(),
            },
            FileContext {
                path: "src/ai/codex_local/rpc.rs".to_owned(),
                symbols: "merge_rate_limits_into_snapshot CodexSnapshot".to_owned(),
            },
        ],
    };
    let out = rank_relevant_files_locally(&req);
    assert_eq!(out.relevant_file_paths, vec!["src/ai/codex_local/rpc.rs"]);
}

#[test]
fn quota_reset_labels_do_not_jitter_on_unused_or_boundary_windows() {
    let now = chrono::Utc::now().timestamp();
    let unused_5h = CodexWindowQuota {
        used_percent: 0.0,
        window_mins: Some(300),
        resets_at_unix: Some(now + 17_999),
    };
    assert_eq!(format_quota_reset_label(&unused_5h), "5h rolling window");

    // Even when active (`> 0%`), 17,999s rounds cleanly to 300m (`5h 0m`) instead of `4h 59m`.
    assert_eq!(format_reset_time(Some(now + 17_999)), "Resets in 5h 0m");

    let mut snap_a = CodexSnapshot {
        five_hour: Some(unused_5h),
        ..CodexSnapshot::default()
    };
    let mut snap_b = snap_a.clone();
    snap_b.five_hour.as_mut().unwrap().resets_at_unix = Some(now + 18_002);
    assert!(snapshots_meaningfully_equal(&snap_a, &snap_b));

    snap_a.five_hour.as_mut().unwrap().used_percent = 12.0;
    assert!(!snapshots_meaningfully_equal(&snap_a, &snap_b));
}
