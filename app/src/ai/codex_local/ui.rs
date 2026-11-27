use chrono::{Local, TimeZone};
use pathfinder_color::ColorU;
use thousands::Separable;
use warp_core::ui::appearance::Appearance;
use warpui::Element;
use warpui::elements::{
    Border, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Empty, Expanded, Flex,
    MainAxisAlignment, MainAxisSize, ParentElement, Radius, Shrinkable, Text, Wrap,
};
use warpui::fonts::{Properties, Weight};

use super::rpc::{CodexSnapshot, CodexWindowQuota, snapshot};
use crate::ui_components::blended_colors;
use crate::ui_components::icons::Icon;

const QUOTA_BAR_HEIGHT: f32 = 6.0;

const ACCENT_GREEN: ColorU = ColorU {
    r: 46,
    g: 184,
    b: 114,
    a: 255,
};
const ACCENT_AMBER: ColorU = ColorU {
    r: 236,
    g: 164,
    b: 58,
    a: 255,
};
const ACCENT_RED: ColorU = ColorU {
    r: 229,
    g: 83,
    b: 75,
    a: 255,
};

fn bar_color(used_pct: f64) -> ColorU {
    if used_pct >= 95.0 {
        ACCENT_RED
    } else if used_pct >= 75.0 {
        ACCENT_AMBER
    } else {
        ACCENT_GREEN
    }
}

pub fn format_reset_time(unix_secs: Option<i64>) -> String {
    let Some(ts) = unix_secs.filter(|&s| s > 0) else {
        return "Rolling window".to_owned();
    };
    let now = chrono::Utc::now().timestamp();
    let diff = ts - now;
    if diff <= 0 {
        return "Resetting soon".to_owned();
    }
    if diff < 3600 {
        return format!("Resets in {}m", (diff / 60).max(1));
    }
    if diff < 86_400 {
        let h = diff / 3600;
        let m = (diff % 3600) / 60;
        return format!("Resets in {h}h {m}m");
    }
    if let Some(dt) = Local.timestamp_opt(ts, 0).single() {
        return format!("Resets {}", dt.format("%a %b %-d, %H:%M"));
    }
    format!("Resets in {}d", diff / 86_400)
}

fn render_progress_bar(used_pct: f64, track_color: ColorU) -> Box<dyn Element> {
    let clamped = (used_pct as f32).clamp(0.0, 100.0);
    let rem = (100.0 - clamped).max(0.0);
    let end_radius = Radius::Pixels(QUOTA_BAR_HEIGHT / 2.0);
    let mut row = Flex::row();

    if clamped > 0.0 {
        let mut r = CornerRadius::with_left(end_radius);
        if rem <= 0.0 {
            r.merge(CornerRadius::with_right(end_radius));
        }
        row.add_child(
            Expanded::new(
                clamped.max(1.0),
                Container::new(Empty::new().finish())
                    .with_background_color(bar_color(used_pct))
                    .with_corner_radius(r)
                    .finish(),
            )
            .finish(),
        );
    }
    if rem > 0.0 {
        let mut r = CornerRadius::with_right(end_radius);
        if clamped <= 0.0 {
            r.merge(CornerRadius::with_left(end_radius));
        }
        row.add_child(
            Expanded::new(
                rem,
                Container::new(Empty::new().finish())
                    .with_background_color(track_color)
                    .with_corner_radius(r)
                    .finish(),
            )
            .finish(),
        );
    }

    ConstrainedBox::new(row.finish())
        .with_height(QUOTA_BAR_HEIGHT)
        .finish()
}

fn render_quota_window_row(
    title: &str,
    quota: &CodexWindowQuota,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let bg = theme.surface_2();
    let font_size = appearance.ui_font_size();
    let reset_text = format_reset_time(quota.resets_at_unix);
    let pct_label = format!("{:.0}% used · {reset_text}", quota.used_percent);

    let top_row = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
        .with_main_axis_size(MainAxisSize::Max)
        .with_child(
            Text::new(title.to_owned(), appearance.ui_font_family(), font_size)
                .with_color(blended_colors::text_sub(theme, bg))
                .finish(),
        )
        .with_child(
            Text::new(pct_label, appearance.ui_font_family(), font_size)
                .with_color(blended_colors::text_main(theme, bg))
                .finish(),
        )
        .finish();

    Flex::column()
        .with_spacing(4.0)
        .with_child(top_row)
        .with_child(render_progress_bar(
            quota.used_percent,
            theme.outline().into_solid(),
        ))
        .finish()
}

/// Renders the live Codex Plan & Quota section inside `UsagePopoverView`.
pub fn render_codex_popover_section(appearance: &Appearance) -> Box<dyn Element> {
    let snap = snapshot();
    let theme = appearance.theme();
    let bg = theme.surface_2();
    let font_size = appearance.ui_font_size();

    let header = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
        .with_main_axis_size(MainAxisSize::Max)
        .with_child(
            Text::new(
                "CODEX PLAN & QUOTA".to_owned(),
                appearance.overline_font_family(),
                appearance.overline_font_size() + 2.0,
            )
            .with_color(blended_colors::text_disabled(theme, bg))
            .finish(),
        )
        .with_child(
            Text::new(snap.plan_badge(), appearance.ui_font_family(), font_size)
                .with_color(blended_colors::text_main(theme, bg))
                .with_style(Properties::default().weight(Weight::Semibold))
                .finish(),
        )
        .finish();

    let mut col = Flex::column().with_spacing(8.0).with_child(header);

    if let Some(ref email) = snap.email {
        col.add_child(
            Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_main_axis_size(MainAxisSize::Max)
                .with_child(
                    Text::new("Account".to_owned(), appearance.ui_font_family(), font_size)
                        .with_color(blended_colors::text_sub(theme, bg))
                        .finish(),
                )
                .with_child(
                    Text::new(email.clone(), appearance.ui_font_family(), font_size)
                        .with_color(blended_colors::text_main(theme, bg))
                        .finish(),
                )
                .finish(),
        );
    }

    if let Some(ref five_h) = snap.five_hour {
        col.add_child(render_quota_window_row("5-hour window", five_h, appearance));
    }
    if let Some(ref week) = snap.weekly {
        col.add_child(render_quota_window_row("7-day window", week, appearance));
    }

    if let Some(tokens) = snap.lifetime_tokens.filter(|&t| t > 0) {
        col.add_child(
            Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_main_axis_size(MainAxisSize::Max)
                .with_child(
                    Text::new(
                        "Codex lifetime tokens".to_owned(),
                        appearance.ui_font_family(),
                        font_size,
                    )
                    .with_color(blended_colors::text_sub(theme, bg))
                    .finish(),
                )
                .with_child(
                    Text::new(
                        tokens.separate_with_commas(),
                        appearance.ui_font_family(),
                        font_size,
                    )
                    .with_color(blended_colors::text_main(theme, bg))
                    .finish(),
                )
                .finish(),
        );
    }

    col.finish()
}

/// Action buttons passed by `BillingAndUsagePageV2View` into the standalone Codex settings view.
pub struct CodexSettingsActionButtons {
    pub sign_in: Box<dyn Element>,
    pub refresh: Box<dyn Element>,
    pub manage_usage: Box<dyn Element>,
    pub sign_out: Box<dyn Element>,
}

fn render_kv_row(
    label: &str,
    value: String,
    appearance: &Appearance,
    sub_color: ColorU,
    val_color: ColorU,
) -> Box<dyn Element> {
    let font_size = appearance.ui_font_size();
    Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
        .with_main_axis_size(MainAxisSize::Max)
        .with_spacing(12.0)
        .with_child(
            Text::new_inline(label.to_owned(), appearance.ui_font_family(), font_size)
                .with_color(sub_color)
                .finish(),
        )
        .with_child(
            Shrinkable::new(
                1.0,
                Text::new_inline(value, appearance.ui_font_family(), font_size)
                    .with_color(val_color)
                    .finish(),
            )
            .finish(),
        )
        .finish()
}

fn render_card_shell(body: Box<dyn Element>, appearance: &Appearance) -> Box<dyn Element> {
    let theme = appearance.theme();
    Container::new(body)
        .with_border(Border::all(1.0).with_border_color(theme.outline().into()))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.0)))
        .with_horizontal_padding(18.0)
        .with_vertical_padding(16.0)
        .with_margin_bottom(16.0)
        .finish()
}

/// Renders the standalone, Warp-Cloud-free Codex & Usage page in Settings.
pub fn render_codex_full_settings_page(
    snap: &CodexSnapshot,
    buttons: CodexSettingsActionButtons,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let text_color = theme.active_ui_text_color();
    let text_solid: ColorU = text_color.into();
    let sub_color = blended_colors::text_sub(theme, theme.surface_1());

    // ── Card 1: Codex Account & Local CLI Connection ─────────────────────
    let status_dot_color = if snap.logged_in {
        ACCENT_GREEN
    } else if snap.cli_path.is_some() {
        ACCENT_AMBER
    } else {
        ACCENT_RED
    };
    let status_dot = ConstrainedBox::new(
        Container::new(Empty::new().finish())
            .with_background_color(status_dot_color)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.0)))
            .finish(),
    )
    .with_width(8.0)
    .with_height(8.0)
    .finish();

    let logo = ConstrainedBox::new(Icon::OpenAILogo.to_warpui_icon(text_color).finish())
        .with_width(18.0)
        .with_height(18.0)
        .finish();

    let header_title = Text::new_inline(
        format!("Codex Local Backend · {}", snap.plan_badge()),
        appearance.ui_font_family(),
        15.0,
    )
    .with_color(text_solid)
    .with_style(Properties::default().weight(Weight::Bold))
    .finish();

    let mut action_row = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(8.0);
    if snap.logged_in {
        action_row.add_child(buttons.refresh);
        action_row.add_child(buttons.manage_usage);
        action_row.add_child(buttons.sign_out);
    } else {
        action_row.add_child(buttons.sign_in);
        action_row.add_child(buttons.refresh);
    }

    let card1_header = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
        .with_main_axis_size(MainAxisSize::Max)
        .with_spacing(12.0)
        .with_child(
            Shrinkable::new(
                1.0,
                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(8.0)
                    .with_child(logo)
                    .with_child(header_title)
                    .with_child(status_dot)
                    .finish(),
            )
            .finish(),
        )
        .with_child(action_row.finish())
        .finish();

    let mut card1 = Flex::column().with_spacing(10.0).with_child(card1_header);

    let auth_label = if snap.logged_in {
        match snap.auth_mode.as_deref() {
            Some("chatgpt") | None => "Connected via ChatGPT OAuth".to_owned(),
            Some("apiKey") | Some("apikey") => "Connected via OpenAI API Key".to_owned(),
            Some(mode) => format!("Connected ({mode})"),
        }
    } else if snap.cli_path.is_none() {
        "Codex CLI not found — install @openai/codex or set WARP_CODEX_BIN".to_owned()
    } else {
        "Signed out — click Sign in with ChatGPT to link your Codex plan".to_owned()
    };
    card1.add_child(render_kv_row(
        "Connection",
        auth_label,
        appearance,
        sub_color,
        text_solid,
    ));

    if let Some(ref email) = snap.email {
        card1.add_child(render_kv_row(
            "Account",
            format!("{email} ({})", snap.plan_badge()),
            appearance,
            sub_color,
            text_solid,
        ));
    }

    let cli_detail = match (&snap.cli_path, &snap.cli_version) {
        (Some(p), Some(v)) => format!("{v} · {}", p.display()),
        (Some(p), None) => p.display().to_string(),
        _ => "Not detected on PATH".to_owned(),
    };
    card1.add_child(render_kv_row(
        "Local binary",
        cli_detail,
        appearance,
        sub_color,
        text_solid,
    ));

    if let Some(ref err) = snap.last_error {
        card1.add_child(
            Text::new_inline(format!("Note: {err}"), appearance.ui_font_family(), 12.0)
                .with_color(theme.ansi_fg_yellow())
                .finish(),
        );
    }

    // ── Card 2: Plan & Rolling Rate-Limit Windows ────────────────────────
    let mut card2 = Flex::column().with_spacing(12.0).with_child(
        Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_main_axis_size(MainAxisSize::Max)
            .with_child(
                Text::new_inline(
                    "Rolling Quota & Token Usage",
                    appearance.ui_font_family(),
                    15.0,
                )
                .with_color(text_solid)
                .with_style(Properties::default().weight(Weight::Bold))
                .finish(),
            )
            .with_child(
                Text::new_inline(snap.plan_badge(), appearance.ui_font_family(), 13.0)
                    .with_color(sub_color)
                    .with_style(Properties::default().weight(Weight::Semibold))
                    .finish(),
            )
            .finish(),
    );

    if snap.five_hour.is_some() || snap.weekly.is_some() {
        if let Some(ref five_h) = snap.five_hour {
            card2.add_child(render_quota_window_row(
                "5-hour rolling limit",
                five_h,
                appearance,
            ));
        }
        if let Some(ref week) = snap.weekly {
            card2.add_child(render_quota_window_row(
                "7-day weekly limit",
                week,
                appearance,
            ));
        }
    } else {
        card2.add_child(
            Text::new(
                if snap.logged_in {
                    "Live rate-limit windows will appear here as soon as Codex reports quota data."
                        .to_owned()
                } else {
                    "Sign in with your ChatGPT account above to load your 5-hour and 7-day rolling Codex limits."
                        .to_owned()
                },
                appearance.ui_font_family(),
                13.0,
            )
            .with_color(sub_color)
            .finish(),
        );
    }

    let mut stats_parts = Vec::new();
    if snap.credits_unlimited {
        stats_parts.push("Plan credits: Included".to_owned());
    } else if let Some(ref bal) = snap.credits_balance {
        stats_parts.push(format!("Credits balance: {bal}"));
    }
    if let Some(lt) = snap.lifetime_tokens.filter(|&t| t > 0) {
        stats_parts.push(format!("Lifetime tokens: {}", lt.separate_with_commas()));
    }
    if let Some(pk) = snap.peak_daily_tokens.filter(|&t| t > 0) {
        stats_parts.push(format!("Peak daily: {}", pk.separate_with_commas()));
    }
    if !stats_parts.is_empty() {
        card2.add_child(
            Text::new_inline(stats_parts.join("  ·  "), appearance.ui_font_family(), 12.0)
                .with_color(sub_color)
                .finish(),
        );
    }

    // ── Card 3: Models Exposed by Codex Plan ─────────────────────────────
    let mut card3 = Flex::column().with_spacing(10.0).with_child(
        Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_main_axis_size(MainAxisSize::Max)
            .with_child(
                Text::new_inline("Models Exposed by Codex", appearance.ui_font_family(), 15.0)
                    .with_color(text_solid)
                    .with_style(Properties::default().weight(Weight::Bold))
                    .finish(),
            )
            .with_child(
                Text::new_inline(
                    format!("{} available", snap.models.len()),
                    appearance.ui_font_family(),
                    12.0,
                )
                .with_color(sub_color)
                .finish(),
            )
            .finish(),
    );

    card3.add_child(
        Text::new(
            "Every AI surface in WarpCodex — Agent Mode, Plan Mode, # command search, inline suggestions, passive error fixes, and code review — runs locally over stdio JSON-RPC through codex app-server. Switch models and reasoning effort directly from the prompt toolbar."
                .to_owned(),
            appearance.ui_font_family(),
            12.5,
        )
        .with_color(sub_color)
        .finish(),
    );

    if !snap.models.is_empty() {
        let mut chips = Wrap::row().with_spacing(8.0).with_run_spacing(8.0);
        for m in &snap.models {
            let label = if m.is_default {
                format!("{} · default", m.display_name)
            } else {
                m.display_name.clone()
            };
            chips.add_child(
                Container::new(
                    Text::new_inline(label, appearance.ui_font_family(), 12.0)
                        .with_color(text_solid)
                        .finish(),
                )
                .with_background(theme.surface_2())
                .with_border(Border::all(1.0).with_border_color(theme.outline().into()))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.0)))
                .with_horizontal_padding(10.0)
                .with_vertical_padding(5.0)
                .finish(),
            );
        }
        card3.add_child(chips.finish());
    }

    Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_child(render_card_shell(card1.finish(), appearance))
        .with_child(render_card_shell(card2.finish(), appearance))
        .with_child(render_card_shell(card3.finish(), appearance))
        .finish()
}

/// Renders the rich Codex CLI status, plan badge, 5h/7d rate-limit windows, and exposed models card
/// for Settings -> Billing & Usage.
pub fn render_codex_settings_overview(
    snap: &CodexSnapshot,
    action_button: Box<dyn Element>,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let text_color = theme.active_ui_text_color();
    let sub_color = blended_colors::text_sub(theme, theme.surface_1());

    let logo = ConstrainedBox::new(Icon::OpenAILogo.to_warpui_icon(text_color).finish())
        .with_width(18.0)
        .with_height(18.0)
        .finish();

    let title_text = format!("Codex Local Backend · {}", snap.plan_badge());
    let title = Text::new_inline(title_text, appearance.ui_font_family(), 15.0)
        .with_color(text_color.into())
        .with_style(Properties::default().weight(Weight::Bold))
        .finish();

    let cli_detail = match (&snap.cli_path, &snap.cli_version) {
        (Some(p), Some(v)) => format!("{v} ({})", p.display()),
        (Some(p), None) => p.display().to_string(),
        _ => "codex binary not found on PATH".to_owned(),
    };
    let account_detail = match &snap.email {
        Some(email) => format!("{email} · {cli_detail}"),
        None => cli_detail,
    };

    let subtitle = Text::new_inline(account_detail, appearance.ui_font_family(), 12.0)
        .with_color(sub_color)
        .finish();

    let left = Flex::column()
        .with_spacing(4.0)
        .with_child(
            Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(8.0)
                .with_child(logo)
                .with_child(title)
                .finish(),
        )
        .with_child(subtitle)
        .finish();

    let top_row = Flex::row()
        .with_child(Shrinkable::new(1.0, left).finish())
        .with_child(action_button)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
        .with_main_axis_size(MainAxisSize::Max)
        .finish();

    let mut body = Flex::column().with_spacing(12.0).with_child(top_row);

    if let Some(ref five_h) = snap.five_hour {
        body.add_child(render_quota_window_row("5-hour rolling limit", five_h, appearance));
    }
    if let Some(ref week) = snap.weekly {
        body.add_child(render_quota_window_row("7-day weekly limit", week, appearance));
    }

    if snap.credits_unlimited || snap.credits_balance.is_some() || snap.lifetime_tokens.is_some() {
        let mut stats_parts = Vec::new();
        if snap.credits_unlimited {
            stats_parts.push("Credits: Unlimited".to_owned());
        } else if let Some(ref bal) = snap.credits_balance {
            stats_parts.push(format!("Credits balance: {bal}"));
        }
        if let Some(lt) = snap.lifetime_tokens.filter(|&t| t > 0) {
            stats_parts.push(format!("Lifetime tokens: {}", lt.separate_with_commas()));
        }
        if let Some(pk) = snap.peak_daily_tokens.filter(|&t| t > 0) {
            stats_parts.push(format!("Peak daily: {}", pk.separate_with_commas()));
        }
        if !stats_parts.is_empty() {
            body.add_child(
                Text::new_inline(stats_parts.join("  ·  "), appearance.ui_font_family(), 12.0)
                    .with_color(sub_color)
                    .finish(),
            );
        }
    }

    if !snap.models.is_empty() {
        let model_names: Vec<String> = snap
            .models
            .iter()
            .map(|m| {
                if m.is_default {
                    format!("{} (default)", m.display_name)
                } else {
                    m.display_name.clone()
                }
            })
            .collect();
        body.add_child(
            Text::new_inline(
                format!("Exposed Codex models: {}", model_names.join(", ")),
                appearance.ui_font_family(),
                12.0,
            )
            .with_color(sub_color)
            .finish(),
        );
    }

    if let Some(ref err) = snap.last_error {
        body.add_child(
            Text::new_inline(format!("Status note: {err}"), appearance.ui_font_family(), 12.0)
                .with_color(theme.ansi_fg_yellow())
                .finish(),
        );
    }

    Container::new(body.finish())
        .with_border(Border::all(1.0).with_border_color(theme.outline().into()))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.0)))
        .with_horizontal_padding(16.0)
        .with_vertical_padding(14.0)
        .with_margin_bottom(16.0)
        .finish()
}
