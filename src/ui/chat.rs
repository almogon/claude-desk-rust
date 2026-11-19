//! Center area: transcript, welcome screen and history view.

use std::collections::HashMap;

use chrono::Local;
use egui::{Align, CollapsingHeader, Frame, Layout, Margin, RichText, ScrollArea, Sense, Ui};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

use super::theme::{self, ACCENT, BG, MUTED, OK, R_BUBBLE, R_CARD, RAISED, RED};
use super::{
    Action, DeckApp, Selected, dot, fmt_cost, fmt_duration_ms, fmt_tokens, input, permission,
    reading_column, short_dir, trunc,
};
use crate::session::SessionKey;
use crate::state::{Item, NoteKind, Session, SessionStatus, TurnSummary};

pub fn show(ui: &mut Ui, app: &mut DeckApp, actions: &mut Vec<Action>) {
    // The input panel must be declared before the central panel.
    if let Some(k) = app.active_key()
        && app.state.session(k).is_some()
    {
        input::show(ui, app, k, actions);
    }

    egui::CentralPanel::default().show(ui, |ui| {
        let DeckApp {
            state,
            selected,
            md,
            shared_md,
            deny_reasons,
            ..
        } = app;
        match selected {
            Some(Selected::Session(k)) if state.session(*k).is_some() => {
                let key = *k;
                let Some(s) = state.session(key) else { return };
                let cache = md.entry(key).or_default();
                ScrollArea::vertical()
                    .id_salt(("chat", key.0))
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        reading_column(ui, |ui| {
                            ui.add_space(12.0);
                            if s.transcript.is_empty() {
                                ui.add_space(20.0);
                                ui.label(theme::muted("Escribe un mensaje abajo para empezar."));
                            }
                            render_items(
                                ui,
                                &s.transcript,
                                "main",
                                cache,
                                Some(key),
                                deny_reasons,
                                actions,
                            );
                            working_line(ui, s);
                            ui.add_space(12.0);
                        });
                    });
            }
            Some(Selected::History(h)) => {
                reading_column(ui, |ui| {
                    ui.add_space(14.0);
                    ui.label(theme::title(trunc(&h.loaded.title, 90)));
                    let cwd = h.entry.cwd.as_deref().unwrap_or("?");
                    ui.horizontal(|ui| {
                        ui.label(theme::muted(short_dir(cwd))).on_hover_text(cwd);
                        ui.label(theme::muted(format!(
                            "· {}",
                            h.entry.modified.format("%d/%m/%Y %H:%M")
                        )));
                        ui.label(theme::muted("· solo lectura"));
                    });
                    ui.add_space(4.0);
                    let btn = egui::Button::new(RichText::new("Reanudar conversación").color(BG))
                        .fill(ACCENT)
                        .stroke(egui::Stroke::NONE);
                    if ui.add(btn).clicked() {
                        actions.push(Action::ResumeHistory);
                    }
                    ui.add_space(6.0);
                    ui.separator();
                });
                ScrollArea::vertical()
                    .id_salt(("history", &h.entry.session_id))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        reading_column(ui, |ui| {
                            render_items(
                                ui,
                                &h.loaded.items,
                                "hist",
                                shared_md,
                                None,
                                deny_reasons,
                                actions,
                            );
                            ui.add_space(12.0);
                        });
                    });
            }
            _ => welcome(ui, actions),
        }
    });
}

fn welcome(ui: &mut Ui, actions: &mut Vec<Action>) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() / 3.0);
        ui.add(
            egui::Image::new(egui::include_image!("../../assets/icon.svg"))
                .fit_to_exact_size(egui::vec2(56.0, 56.0)),
        );
        ui.add_space(10.0);
        ui.label(theme::title("Claude Deck"));
        ui.label(theme::muted(
            "Selecciona una conversación del lateral o empieza una nueva.",
        ));
        ui.add_space(12.0);
        let btn = egui::Button::new(RichText::new("Nueva conversación").color(BG))
            .fill(ACCENT)
            .stroke(egui::Stroke::NONE);
        if ui.add(btn).clicked() {
            actions.push(Action::NewConversation);
        }
        ui.add_space(6.0);
        ui.label(theme::muted(
            "Ctrl+N nueva · Esc detiene · Ctrl+F busca en el historial",
        ));
    });
}

/// Live row at the end of the transcript while Claude is working.
fn working_line(ui: &mut Ui, s: &Session) {
    if !matches!(s.status, SessionStatus::Starting | SessionStatus::Running) {
        return;
    }
    let started = s.transcript.iter().rev().find_map(|i| match i {
        Item::UserText { at, .. } => Some(*at),
        _ => None,
    });
    let mut text = "Claude está trabajando".to_owned();
    if let Some(at) = started {
        let secs = Local::now().signed_duration_since(at).num_seconds().max(0);
        text.push_str(&format!(" · {secs} s"));
    }
    let running_tool = s.transcript.iter().rev().find_map(|i| match i {
        Item::ToolUse {
            name,
            input,
            result: None,
            ..
        } => Some((name, input)),
        _ => None,
    });
    if let Some((name, input)) = running_tool {
        let sum = tool_summary(name, input);
        if sum.is_empty() {
            text.push_str(&format!(" · {name}"));
        } else {
            text.push_str(&format!(" · {name}: {}", trunc(&sum, 50)));
        }
    }
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
        ui.label(RichText::new(text).small().color(ACCENT));
    });
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(500));
}

/// Renders a list of items (main transcript, subagent or history).
/// `interactive` is the session whose permissions can be answered.
pub fn render_items(
    ui: &mut Ui,
    items: &[Item],
    id_prefix: &str,
    cache: &mut CommonMarkCache,
    interactive: Option<SessionKey>,
    reasons: &mut HashMap<String, String>,
    actions: &mut Vec<Action>,
) {
    let mut i = 0;
    while i < items.len() {
        if matches!(items[i], Item::ToolUse { .. }) {
            let start = i;
            while i < items.len() && matches!(items[i], Item::ToolUse { .. }) {
                i += 1;
            }
            ui.push_id((id_prefix, start), |ui| tool_run(ui, &items[start..i]));
        } else {
            ui.push_id((id_prefix, i), |ui| {
                item_view(ui, &items[i], cache, interactive, reasons, actions);
            });
            i += 1;
        }
        ui.add_space(4.0);
    }
}

/// One tool row, or a collapsible group for several consecutive tools.
fn tool_run(ui: &mut Ui, run: &[Item]) {
    if run.len() == 1 {
        tool_row(ui, &run[0]);
        return;
    }
    let running = run
        .iter()
        .any(|t| matches!(t, Item::ToolUse { result: None, .. }));
    let failed = run.iter().any(|t| {
        matches!(
            t,
            Item::ToolUse {
                result: Some((_, true)),
                ..
            }
        )
    });
    let mut title = format!("{} herramientas", run.len());
    if running {
        title.push_str(" · ejecutando");
    } else if failed {
        title.push_str(" · con errores");
    }
    CollapsingHeader::new(RichText::new(title).small().color(MUTED))
        .default_open(running)
        .show(ui, |ui| {
            for t in run {
                tool_row(ui, t);
            }
        });
}

fn tool_row(ui: &mut Ui, item: &Item) {
    let Item::ToolUse {
        id,
        name,
        input,
        result,
    } = item
    else {
        return;
    };
    let color = match result {
        None => ACCENT,
        Some((_, true)) => RED,
        Some((_, false)) => OK,
    };
    let state_id = ui.make_persistent_id(("tool_open", id));
    let open = ui.data(|d| d.get_temp::<bool>(state_id)).unwrap_or(false);
    let summary = tool_summary(name, input);
    let row = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if result.is_none() {
            ui.add(egui::Spinner::new().size(12.0).color(color));
        } else {
            dot(ui, color, 3.5);
        }
        ui.label(RichText::new(name).monospace().color(theme::TEXT));
        if !summary.is_empty() {
            ui.label(RichText::new(summary).monospace().color(MUTED));
        }
    });
    let resp = ui
        .interact(row.response.rect, state_id.with("click"), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        ui.data_mut(|d| d.insert_temp(state_id, !open));
    }
    if open {
        Frame::new()
            .fill(RAISED)
            .corner_radius(R_CARD)
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let pretty = serde_json::to_string_pretty(input).unwrap_or_default();
                ui.label(
                    RichText::new(trunc(&pretty, 1500))
                        .monospace()
                        .small()
                        .color(MUTED),
                );
                match result {
                    Some((text, is_err)) => {
                        let c = if *is_err { RED } else { theme::TEXT };
                        ui.label(
                            RichText::new(trunc(text, 2000))
                                .monospace()
                                .small()
                                .color(c),
                        );
                    }
                    None => {
                        ui.label(theme::muted("Ejecutando…"));
                    }
                }
            });
    }
}

fn item_view(
    ui: &mut Ui,
    item: &Item,
    cache: &mut CommonMarkCache,
    interactive: Option<SessionKey>,
    reasons: &mut HashMap<String, String>,
    actions: &mut Vec<Action>,
) {
    match item {
        Item::UserText {
            text, attachments, ..
        } => {
            let max = ui.available_width() * 0.82;
            ui.with_layout(Layout::top_down(Align::Max), |ui| {
                Frame::new()
                    .fill(RAISED)
                    .corner_radius(R_BUBBLE)
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_max_width(max);
                        if !attachments.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                for a in attachments {
                                    Frame::new()
                                        .fill(BG)
                                        .corner_radius(R_CARD)
                                        .inner_margin(Margin::symmetric(8, 2))
                                        .show(ui, |ui| {
                                            ui.label(RichText::new(&a.label).small().color(MUTED));
                                        });
                                }
                            });
                        }
                        if !text.is_empty() {
                            ui.label(text);
                        }
                    });
            });
        }
        Item::AssistantText { text, .. } => {
            CommonMarkViewer::new().show(ui, cache, text);
        }
        Item::ToolUse { .. } => tool_row(ui, item),
        Item::PermissionRequest(p) => {
            permission::card(ui, p, interactive, cache, reasons, actions);
        }
        Item::Note { kind, text, .. } => {
            let color = if *kind == NoteKind::Error { RED } else { MUTED };
            ui.label(RichText::new(text).small().color(color));
        }
        Item::TurnSummary(t) => {
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.label(RichText::new(turn_short(t)).small().color(MUTED))
                    .on_hover_text(turn_text(t));
            });
        }
    }
}

/// One-line turn summary: duration · tokens · cost.
fn turn_short(t: &TurnSummary) -> String {
    let mut s = format!(
        "{} · {} tokens",
        fmt_duration_ms(t.duration_ms),
        fmt_tokens(t.input_tokens + t.output_tokens)
    );
    if t.total_cost_usd > 0.0 {
        s.push_str(&format!(" · {}", fmt_cost(t.turn_cost_usd)));
    }
    if t.is_error {
        s.push_str(" · con error");
    }
    s
}

/// Full turn detail (tooltip).
fn turn_text(t: &TurnSummary) -> String {
    let mut s = format!(
        "Duración {}\nEntrada {} · salida {}\nCaché leída {} · escrita {}",
        fmt_duration_ms(t.duration_ms),
        fmt_tokens(t.input_tokens),
        fmt_tokens(t.output_tokens),
        fmt_tokens(t.cache_read_tokens),
        fmt_tokens(t.cache_creation_tokens),
    );
    if t.total_cost_usd > 0.0 {
        s.push_str(&format!(
            "\nCoste {} (acumulado {})",
            fmt_cost(t.turn_cost_usd),
            fmt_cost(t.total_cost_usd)
        ));
    }
    if t.is_error {
        s.push_str("\nEl turno terminó con error");
    }
    s
}

/// Short human description of a tool call (command, path, ...).
pub fn tool_summary(name: &str, input: &serde_json::Value) -> String {
    let _ = name;
    for k in [
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "description",
        "query",
        "prompt",
    ] {
        if let Some(v) = input.get(k).and_then(|v| v.as_str()) {
            return trunc(&v.split_whitespace().collect::<Vec<_>>().join(" "), 80);
        }
    }
    String::new()
}
