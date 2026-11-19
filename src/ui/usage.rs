//! Bottom panel: tabs "Subagentes" / "Usage", collapsible.

use chrono::{DateTime, Local};
use egui::{Color32, Frame, Margin, ProgressBar, RichText, ScrollArea, Stroke, Ui};

use super::theme::{self, AMBER, LINE, MUTED, OK, RED, SURFACE};
use super::{Action, BottomTab, DeckApp, fmt_cost, fmt_tokens, subagents};
use crate::state::{RateWindowState, TokenTotals};

pub fn show_bottom(ui: &mut Ui, app: &mut DeckApp, actions: &mut Vec<Action>) {
    let collapsed = app.settings.bottom_collapsed;
    let frame = Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, LINE))
        .inner_margin(Margin::symmetric(14, 6));
    // Different ids so the stored height of the open panel survives folding.
    let panel = if collapsed {
        egui::Panel::bottom("bottom_panel_folded").resizable(false)
    } else {
        egui::Panel::bottom("bottom_panel")
            .resizable(true)
            .default_size(210.0)
            .size_range(90.0..=520.0)
    };
    let active_running = subagents::running_count(app);
    let usage_label = match app.state.rate_limits().five_hour.as_ref() {
        Some(w) => format!("Usage · 5 h {:.0} %", w.utilization.clamp(0.0, 1.0) * 100.0),
        None => "Usage".to_owned(),
    };
    let sub_label = if active_running > 0 {
        format!(
            "Subagentes · {active_running} {}",
            if active_running == 1 {
                "activo"
            } else {
                "activos"
            }
        )
    } else {
        "Subagentes".to_owned()
    };
    panel.frame(frame).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut app.settings.bottom_tab,
                BottomTab::Subagents,
                sub_label,
            );
            ui.selectable_value(&mut app.settings.bottom_tab, BottomTab::Usage, usage_label);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let text = if collapsed { "Desplegar" } else { "Plegar" };
                if ui
                    .small_button(RichText::new(text).color(MUTED))
                    .on_hover_text("Plegar o desplegar el panel inferior")
                    .clicked()
                {
                    app.settings.bottom_collapsed = !collapsed;
                }
            });
        });
        if collapsed {
            return;
        }
        ui.separator();
        match app.settings.bottom_tab {
            BottomTab::Subagents => subagents::show(ui, app, actions),
            BottomTab::Usage => usage_tab(ui, app, actions),
        }
    });
}

fn bar_color(u: f64) -> Color32 {
    if u < 0.6 {
        OK
    } else if u < 0.85 {
        AMBER
    } else {
        RED
    }
}

fn reset_text(dt: DateTime<Local>) -> String {
    let now = Local::now();
    let rem = dt.signed_duration_since(now);
    let when = if dt.date_naive() == now.date_naive() {
        format!("a las {}", dt.format("%H:%M"))
    } else {
        format!("el {} a las {}", dt.format("%d/%m"), dt.format("%H:%M"))
    };
    if rem.num_minutes() <= 0 {
        format!("se reinicia {when}")
    } else if rem.num_hours() < 48 {
        format!(
            "se reinicia en {} h {} min ({when})",
            rem.num_hours(),
            rem.num_minutes() % 60
        )
    } else {
        format!("se reinicia en {} días ({when})", rem.num_days())
    }
}

fn bar(ui: &mut Ui, label: &str, w: Option<&RateWindowState>) {
    ui.label(theme::semibold(label));
    match w {
        None => {
            ui.label(theme::muted("sin datos"));
        }
        Some(w) => {
            let u = w.utilization.clamp(0.0, 1.0);
            ui.add(
                ProgressBar::new(u as f32)
                    .fill(bar_color(u))
                    .desired_height(14.0)
                    .text(RichText::new(format!("{:.0} %", u * 100.0)).small()),
            );
            ui.label(theme::muted(reset_text(w.resets_at)));
        }
    }
}

fn usage_tab(ui: &mut Ui, app: &mut DeckApp, actions: &mut Vec<Action>) {
    ScrollArea::vertical()
        .id_salt("usage_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.columns(2, |cols| {
                // Left: plan limits.
                let ui = &mut cols[0];
                let rl = app.state.rate_limits();
                if rl.five_hour.is_none() && rl.seven_day.is_none() {
                    ui.label(theme::muted(
                        "Aún no hay datos de límites: aparecen tras la primera respuesta de Claude \
                         (o pulsa «Actualizar /usage»).",
                    ));
                } else {
                    bar(ui, "Sesión de 5 h", rl.five_hour.as_ref());
                    ui.add_space(6.0);
                    bar(ui, "Semana (7 días)", rl.seven_day.as_ref());
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!app.usage_busy, egui::Button::new("Actualizar /usage"))
                        .clicked()
                    {
                        actions.push(Action::RefreshUsage);
                    }
                    if app.usage_busy {
                        ui.add(egui::Spinner::new().size(14.0));
                    }
                });
                if let Some(e) = &app.usage_error {
                    ui.label(RichText::new(e).color(RED));
                }
                if let Some(t) = app.state.usage_text() {
                    egui::CollapsingHeader::new(
                        RichText::new(format!("Salida de /usage · {}", t.at.format("%H:%M:%S")))
                            .small()
                            .color(MUTED),
                    )
                    .show(ui, |ui| {
                        ui.label(RichText::new(&t.text).monospace());
                    });
                }

                // Right: token and cost table.
                let ui = &mut cols[1];
                egui::Grid::new("usage_grid")
                    .num_columns(3)
                    .spacing([18.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("");
                        ui.label(theme::semibold("Conversación"));
                        ui.label(theme::semibold("Hoy"));
                        ui.end_row();
                        let conv = app
                            .active_key()
                            .and_then(|k| app.state.session(k))
                            .map(|s| s.usage);
                        let day = app.state.day_usage();
                        let labels = ["Entrada", "Salida", "Caché leída", "Coste", "Turnos"];
                        let cell = |sel: usize, t: Option<(TokenTotals, f64, u32)>| -> String {
                            match t {
                                None => "—".to_owned(),
                                Some((t, c, n)) => match sel {
                                    0 => fmt_tokens(t.input),
                                    1 => fmt_tokens(t.output),
                                    2 => fmt_tokens(t.cache_read),
                                    3 => fmt_cost(c),
                                    _ => n.to_string(),
                                },
                            }
                        };
                        let conv = conv.map(|u| (u.tokens, u.cost_usd, u.turns));
                        let day = day.map(|d| (d.tokens, d.cost_usd, d.turns));
                        for (i, l) in labels.iter().enumerate() {
                            ui.label(theme::muted(*l));
                            ui.label(cell(i, conv));
                            ui.label(cell(i, day));
                            ui.end_row();
                        }
                    });
                ui.label(theme::muted("«Hoy» cuenta solo esta ejecución de la app."));
            });
        });
}
