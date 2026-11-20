//! "Subagentes" tab of the bottom panel.

use std::collections::HashMap;

use chrono::Local;
use egui::{Frame, Margin, RichText, ScrollArea, Ui};
use egui_extras::{Column, TableBuilder};

use super::chat::render_items;
use super::theme::{self, ACCENT, MUTED, OK, R_CARD, RAISED, RED};
use super::{Action, DeckApp, dot, fmt_duration_ms, fmt_tokens, trunc};
use crate::state::{Session, Subagent, SubagentStatus};

/// Subagents of the current view (active session, or all).
fn visible(app: &DeckApp) -> Vec<(&Session, &Subagent)> {
    let mut list = Vec::new();
    if app.settings.all_subagents {
        for s in app.state.sessions() {
            list.extend(s.subagents.iter().map(|a| (s, a)));
        }
    } else if let Some(s) = app.active_key().and_then(|k| app.state.session(k)) {
        list.extend(s.subagents.iter().map(|a| (s, a)));
    }
    list
}

/// Number of running subagents (background included) in the current view.
pub fn running_count(app: &DeckApp) -> usize {
    visible(app)
        .iter()
        .filter(|(_, a)| a.status == SubagentStatus::Running)
        .count()
}

/// Elapsed milliseconds of a subagent (until now if still running).
fn elapsed_ms(a: &Subagent) -> u64 {
    a.ended_at
        .unwrap_or_else(Local::now)
        .signed_duration_since(a.started_at)
        .num_milliseconds()
        .max(0) as u64
}

/// Type label plus a small "bg" tag for background subagents.
fn type_cell(ui: &mut Ui, a: &Subagent) {
    ui.label(a.subagent_type.as_deref().unwrap_or("agent"));
    if a.background {
        ui.label(RichText::new("bg").small().color(ACCENT));
    }
}

/// Model name without the `claude-` prefix.
fn short_model(m: Option<&str>) -> String {
    match m {
        Some(m) => m.strip_prefix("claude-").unwrap_or(m).to_owned(),
        None => "—".to_owned(),
    }
}

/// Toggles the persistent selection when a row is clicked.
fn toggle(sel: &mut String, id: &str) {
    *sel = if sel == id {
        String::new()
    } else {
        id.to_owned()
    };
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
    ui.checkbox(&mut settings.all_subagents, "All sessions");
    let all = settings.all_subagents;

    let mut list: Vec<(&Session, &Subagent)> = Vec::new();
    if all {
        for s in state.sessions() {
            list.extend(s.subagents.iter().map(|a| (s, a)));
        }
    } else if let Some(s) = active.and_then(|k| state.session(k)) {
        list.extend(s.subagents.iter().map(|a| (s, a)));
    }

    let running: Vec<(&Session, &Subagent)> = list
        .iter()
        .filter(|(_, a)| a.status == SubagentStatus::Running)
        .copied()
        .collect();
    let mut finished: Vec<(&Session, &Subagent)> = list
        .iter()
        .filter(|(_, a)| a.status != SubagentStatus::Running)
        .copied()
        .collect();
    finished.sort_by_key(|(_, a)| std::cmp::Reverse(a.ended_at));

    let sel_id = ui.make_persistent_id("subagent_selected");
    let mut sel: String = ui.data(|d| d.get_temp(sel_id)).unwrap_or_default();
    if !list.iter().any(|(_, a)| a.tool_use_id == sel) {
        sel.clear();
    }

    ScrollArea::vertical()
        .id_salt("subagents_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // --- Active ---
            if running.is_empty() {
                ui.label(theme::muted("No subagents running"));
            } else {
                ui.push_id("subagents_active", |ui| {
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
                                "Type",
                                "Description",
                                "Activity",
                                "Duration",
                                "Tools",
                            ] {
                                h.col(|ui| {
                                    ui.label(theme::muted(t));
                                });
                            }
                        })
                        .body(|mut body| {
                            for (s, a) in &running {
                                let ms = elapsed_ms(a);
                                body.row(26.0, |mut row| {
                                    row.set_selected(sel == a.tool_use_id);
                                    row.col(|ui| {
                                        ui.add(egui::Spinner::new().size(12.0).color(ACCENT));
                                    });
                                    row.col(|ui| type_cell(ui, a));
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
                                        ui.label(
                                            RichText::new(a.tool_count.to_string()).color(MUTED),
                                        );
                                    });
                                    if row.response().clicked() {
                                        toggle(&mut sel, &a.tool_use_id);
                                    }
                                });
                            }
                        });
                });
            }

            // --- Finished ---
            if !finished.is_empty() {
                ui.add_space(6.0);
                let sum: u64 = finished.iter().map(|(_, a)| a.tokens_total()).sum();
                let title = format!("Finished · {} · {} tokens", finished.len(), fmt_tokens(sum));
                egui::CollapsingHeader::new(RichText::new(title).small().color(MUTED))
                    .id_salt("subagents_finished_header")
                    .default_open(false)
                    .show(ui, |ui| {
                        finished_section(ui, &finished, all, &mut sel);
                    });
            }

            // --- Detail card ---
            if let Some((_, a)) = list.iter().find(|(_, a)| a.tool_use_id == sel) {
                ui.add_space(8.0);
                Frame::new()
                    .fill(RAISED)
                    .corner_radius(R_CARD)
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(theme::semibold(trunc(&a.description, 120)));
                        let t = a.tokens();
                        ui.push_id("subagent_detail_stats", |ui| {
                            egui::Grid::new("subagent_detail_grid")
                                .num_columns(8)
                                .spacing([16.0, 2.0])
                                .show(ui, |ui| {
                                    for h in [
                                        "Model",
                                        "Input",
                                        "Output",
                                        "Cache read",
                                        "Cache created",
                                        "Total CLI",
                                        "Duration",
                                        "Tools",
                                    ] {
                                        ui.label(theme::muted(h));
                                    }
                                    ui.end_row();
                                    ui.label(short_model(a.model.as_deref()));
                                    ui.label(fmt_tokens(t.input));
                                    ui.label(fmt_tokens(t.output));
                                    ui.label(fmt_tokens(t.cache_read));
                                    ui.label(fmt_tokens(t.cache_creation));
                                    ui.label(
                                        a.total_tokens
                                            .map(fmt_tokens)
                                            .unwrap_or_else(|| "—".to_owned()),
                                    );
                                    ui.label(fmt_duration_ms(elapsed_ms(a)));
                                    ui.label(a.tool_count.to_string());
                                    ui.end_row();
                                });
                        });
                        ui.separator();
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

/// Body of the "Acabados" collapsing section: per-type summary + table.
fn finished_section(ui: &mut Ui, finished: &[(&Session, &Subagent)], all: bool, sel: &mut String) {
    // Per type: (count, tokens, total duration ms).
    let mut by_type: Vec<(String, u64, u64, u64)> = Vec::new();
    for (_, a) in finished {
        let ty = a.subagent_type.as_deref().unwrap_or("agent");
        let tk = a.tokens_total();
        let ms = elapsed_ms(a);
        if let Some(e) = by_type.iter_mut().find(|e| e.0 == ty) {
            e.1 += 1;
            e.2 += tk;
            e.3 += ms;
        } else {
            by_type.push((ty.to_owned(), 1, tk, ms));
        }
    }
    by_type.sort_by_key(|t| std::cmp::Reverse(t.2));

    ui.push_id("subagents_by_type", |ui| {
        egui::Grid::new("subagents_by_type_grid")
            .num_columns(5)
            .spacing([16.0, 2.0])
            .striped(true)
            .show(ui, |ui| {
                for h in ["Type", "No.", "Tokens", "Avg tokens", "Avg duration"] {
                    ui.label(theme::muted(h));
                }
                ui.end_row();
                for (ty, n, tk, ms) in &by_type {
                    ui.label(ty);
                    ui.label(n.to_string());
                    ui.label(fmt_tokens(*tk));
                    ui.label(fmt_tokens(tk / n));
                    ui.label(fmt_duration_ms(ms / n));
                    ui.end_row();
                }
            });
    });
    ui.add_space(6.0);

    ui.push_id("subagents_finished", |ui| {
        TableBuilder::new(ui)
            .striped(true)
            .resizable(false)
            .vscroll(false)
            .sense(egui::Sense::click())
            .column(Column::exact(24.0))
            .column(Column::exact(120.0))
            .column(Column::remainder().at_least(140.0).clip(true))
            .column(Column::exact(130.0).clip(true))
            .column(Column::exact(90.0))
            .column(Column::exact(60.0))
            .column(Column::exact(80.0))
            .header(22.0, |mut h| {
                for t in [
                    "",
                    "Type",
                    "Description",
                    "Model",
                    "Duration",
                    "Tools",
                    "Tokens",
                ] {
                    h.col(|ui| {
                        ui.label(theme::muted(t));
                    });
                }
            })
            .body(|mut body| {
                for (s, a) in finished {
                    body.row(26.0, |mut row| {
                        row.set_selected(*sel == a.tool_use_id);
                        row.col(|ui| {
                            if a.status == SubagentStatus::Failed {
                                dot(ui, RED, 4.0);
                            } else {
                                dot(ui, OK, 4.0);
                            }
                        });
                        row.col(|ui| type_cell(ui, a));
                        row.col(|ui| {
                            let mut d = trunc(&a.description, 90);
                            if all {
                                d.push_str(&format!(" · {}", trunc(&s.title, 20)));
                            }
                            ui.label(d);
                        });
                        row.col(|ui| {
                            ui.label(theme::muted(short_model(a.model.as_deref())));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(fmt_duration_ms(elapsed_ms(a))).color(MUTED));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(a.tool_count.to_string()).color(MUTED));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(fmt_tokens(a.tokens_total())).color(MUTED));
                        });
                        if row.response().clicked() {
                            toggle(sel, &a.tool_use_id);
                        }
                    });
                }
            });
    });
}
