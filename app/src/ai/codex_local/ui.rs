use chrono::{Local, TimeZone};
use pathfinder_color::ColorU;
use thousands::Separable;
use warp_core::ui::appearance::Appearance;
use warpui::Element;
use warpui::elements::{
    Border, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Empty, Expanded, Flex,
    MainAxisAlignment, MainAxisSize, ParentElement, Radius, Shrinkable, Text,
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
