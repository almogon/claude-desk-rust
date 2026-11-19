//! "Subagentes" tab of the bottom panel.

use std::collections::HashMap;

use chrono::Local;
use egui::{Frame, Margin, RichText, ScrollArea, Ui};
use egui_extras::{Column, TableBuilder};

use super::chat::render_items;
use super::theme::{self, ACCENT, MUTED, OK, R_CARD, RAISED, RED};
use super::{Action, DeckApp, dot, fmt_duration_ms, trunc};
use crate::state::{Session, Subagent, SubagentStatus};

/// How long a finished subagent stays listed (green/red dot) before vanishing.
const LINGER_MS: i64 = 3000;

/// Milliseconds a finished subagent still has to be shown (0 if gone/running).
fn linger_left_ms(a: &Subagent) -> i64 {
    match (a.status, a.ended_at) {
        (SubagentStatus::Running, _) | (_, None) => 0,
        (_, Some(end)) => {
            (LINGER_MS - Local::now().signed_duration_since(end).num_milliseconds()).max(0)
        }
    }
}

/// Running subagents, plus those that finished less than 3 s ago.
fn is_shown(a: &Subagent) -> bool {
    a.status == SubagentStatus::Running || linger_left_ms(a) > 0
}

/// Subagents shown for the current view (active session, or all).
fn visible(app: &DeckApp) -> Vec<(&Session, &Subagent)> {
    let mut list = Vec::new();
    if app.settings.all_subagents {
        for s in app.state.sessions() {
            list.extend(s.subagents.iter().filter(|a| is_shown(a)).map(|a| (s, a)));
        }
    } else if let Some(s) = app.active_key().and_then(|k| app.state.session(k)) {
        list.extend(s.subagents.iter().filter(|a| is_shown(a)).map(|a| (s, a)));
    }
    list
}

/// Number of running subagents in the current view (for the tab label).
pub fn running_count(app: &DeckApp) -> usize {
    visible(app)
        .iter()
        .filter(|(_, a)| a.status == SubagentStatus::Running)
        .count()
}

pub fn show(ui: &mut Ui, app: &mut DeckApp, actions: &mut Vec<Action>) {
    let DeckApp {
        state,
        settings,
        shared_md,
        selected,
        ..
    } = app;
    let active = match selected {
        Some(super::Selected::Session(k)) => Some(*k),
        _ => None,
    };
    ui.checkbox(&mut settings.all_subagents, "Todas las sesiones");

    let mut list: Vec<(&Session, &Subagent)> = Vec::new();
    if settings.all_subagents {
        for s in state.sessions() {
            list.extend(s.subagents.iter().filter(|a| is_shown(a)).map(|a| (s, a)));
        }
    } else if let Some(s) = active.and_then(|k| state.session(k)) {
        list.extend(s.subagents.iter().filter(|a| is_shown(a)).map(|a| (s, a)));
    }

    // Wake up when the last "just finished" subagent is due to disappear.
    if let Some(ms) = list
        .iter()
        .map(|(_, a)| linger_left_ms(a))
        .filter(|m| *m > 0)
        .min()
    {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(ms as u64 + 50));
    }

    let sel_id = ui.make_persistent_id("subagent_selected");
    let mut sel: String = ui.data(|d| d.get_temp(sel_id)).unwrap_or_default();
    if !list.iter().any(|(_, a)| a.tool_use_id == sel) {
        sel.clear();
    }

    if list.is_empty() {
        ui.data_mut(|d| d.insert_temp(sel_id, sel));
        // Fill the whole panel so its (user-resized) height does not change.
        ui.allocate_ui_with_layout(
            ui.available_size(),
            egui::Layout::centered_and_justified(egui::Direction::TopDown),
            |ui| {
                ui.label(theme::muted("Ningún subagente trabajando"));
            },
        );
        return;
    }
    let all = settings.all_subagents;

    ScrollArea::vertical()
        .id_salt("subagents_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            TableBuilder::new(ui)
                .striped(true)
                .resizable(false)
                .vscroll(false)
                .sense(egui::Sense::click())
                .column(Column::exact(24.0))
                .column(Column::exact(120.0))
                .column(Column::remainder().at_least(140.0).clip(true))
                .column(Column::exact(210.0).clip(true))
                .column(Column::exact(90.0))
                .column(Column::exact(80.0))
                .header(22.0, |mut h| {
                    for t in [
                        "",
                        "Tipo",
                        "Descripción",
                        "Actividad",
                        "Duración",
                        "Herram.",
                    ] {
                        h.col(|ui| {
                            ui.label(theme::muted(t));
                        });
                    }
                })
                .body(|mut body| {
                    for (s, a) in &list {
                        let ms = a
                            .ended_at
                            .unwrap_or_else(Local::now)
                            .signed_duration_since(a.started_at)
                            .num_milliseconds()
                            .max(0) as u64;
                        body.row(26.0, |mut row| {
                            row.set_selected(sel == a.tool_use_id);
                            row.col(|ui| match a.status {
                                SubagentStatus::Running => {
                                    ui.add(egui::Spinner::new().size(12.0).color(ACCENT));
                                }
                                SubagentStatus::Completed => {
                                    dot(ui, OK, 4.0);
                                }
                                SubagentStatus::Failed => {
                                    dot(ui, RED, 4.0);
                                }
                            });
                            row.col(|ui| {
                                ui.label(a.subagent_type.as_deref().unwrap_or("agente"));
                            });
                            row.col(|ui| {
                                let mut d = trunc(&a.description, 90);
                                if all {
                                    d.push_str(&format!(" · {}", trunc(&s.title, 20)));
                                }
                                ui.label(d);
                            });
                            row.col(|ui| {
                                ui.label(theme::muted(
                                    a.last_activity
                                        .as_deref()
                                        .map(|l| trunc(l, 40))
                                        .unwrap_or_default(),
                                ));
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(fmt_duration_ms(ms)).color(MUTED));
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(a.tool_count.to_string()).color(MUTED));
                            });
                            if row.response().clicked() {
                                sel = if sel == a.tool_use_id {
                                    String::new()
                                } else {
                                    a.tool_use_id.clone()
                                };
                            }
                        });
                    }
                });

            if let Some((_, a)) = list.iter().find(|(_, a)| a.tool_use_id == sel) {
                ui.add_space(8.0);
                Frame::new()
                    .fill(RAISED)
                    .corner_radius(R_CARD)
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(theme::semibold(trunc(&a.description, 120)));
                        if let Some(sum) = &a.summary {
                            ui.label(theme::muted(trunc(sum, 600)));
                            ui.separator();
                        }
                        let mut unused = HashMap::new();
                        render_items(
                            ui,
                            &a.items,
                            &a.tool_use_id,
                            shared_md,
                            None,
                            &mut unused,
                            actions,
                        );
                    });
            }
        });
    ui.data_mut(|d| d.insert_temp(sel_id, sel));
}
