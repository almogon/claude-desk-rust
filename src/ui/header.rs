//! Top bar: brand, active conversation + state chip, model / mode / folder chips.

use egui::{Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Ui, vec2};

use super::theme::{self, ACCENT, AMBER, LINE, MUTED, OK, R_CHIP, RED, SURFACE, TEXT};
use super::{Action, DeckApp, FolderTarget, MODELS, MODES, Selected, dot, short_dir, trunc};

/// Short label of a permission mode for its chip.
fn mode_short(mode: &str) -> &'static str {
    match mode {
        "default" => "Preguntar",
        "acceptEdits" => "Aceptar ediciones",
        "plan" => "Plan",
        "bypassPermissions" => "Sin permisos",
        _ => "Modo",
    }
}

/// Pill-shaped dropdown. Returns the button response (for tooltips).
fn chip(ui: &mut Ui, label: &str, color: Color32, menu: impl FnOnce(&mut Ui)) -> egui::Response {
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        for s in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
            s.corner_radius = CornerRadius::same(R_CHIP);
        }
        ui.spacing_mut().button_padding = vec2(10.0, 3.0);
        ui.menu_button(RichText::new(format!("{label} ▾")).color(color), menu)
            .response
    })
    .inner
}

pub fn show(ui: &mut Ui, app: &mut DeckApp, actions: &mut Vec<Action>) {
    let active = app.active_key();
    let live = active.filter(|k| app.is_alive(*k));
    let (real_model, live_mode, folder, status, worktree) = {
        let s = active.and_then(|k| app.state.session(k));
        (
            s.and_then(|s| s.model.clone()),
            live.and_then(|k| app.state.session(k))
                .and_then(|s| s.mode.clone()),
            s.map(|s| s.cwd.clone()),
            s.map(|s| s.status.clone()),
            s.and_then(|s| s.worktree.clone()),
        )
    };
    let conv_title: Option<String> = match &app.selected {
        Some(Selected::Session(k)) => app.state.session(*k).map(|s| {
            if s.title.is_empty() {
                "Nueva conversación".to_owned()
            } else {
                s.title.clone()
            }
        }),
        Some(Selected::History(h)) => Some(h.loaded.title.clone()),
        None => None,
    };
    let next_conv = "Se aplicará a la próxima conversación.";

    egui::Panel::top("header")
        .frame(
            Frame::new()
                .fill(SURFACE)
                .stroke(egui::Stroke::new(1.0, LINE))
                .inner_margin(Margin::symmetric(14, 8)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::Image::new(egui::include_image!("../../assets/icon.svg"))
                        .fit_to_exact_size(vec2(22.0, 22.0)),
                );
                ui.label(theme::semibold("Claude").size(17.0).color(ACCENT));

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // Diff panel toggle (rightmost)
                    let open = app.settings.diff_open;
                    if ui
                        .add(egui::Button::new("Cambios").selected(open))
                        .on_hover_text("Mostrar u ocultar los cambios del repositorio")
                        .clicked()
                    {
                        actions.push(Action::ToggleDiff);
                    }

                    // Folder
                    let path = folder
                        .clone()
                        .unwrap_or_else(|| app.settings.folder.clone());
                    // A conversation with nothing sent yet is restarted in the new
                    // folder; one already started keeps its folder and the pick
                    // opens a new conversation instead.
                    let target = match active {
                        None => FolderTarget::Default,
                        Some(k) if app.is_fresh(k) => FolderTarget::Replace(k),
                        Some(_) => FolderTarget::NewConversation,
                    };
                    chip(ui, &trunc(&short_dir(&path), 22), TEXT, |ui| {
                        ui.label(theme::muted(path.clone()));
                        ui.separator();
                        let (label, tip) = match target {
                            FolderTarget::NewConversation => (
                                "Nueva conversación en otra carpeta…",
                                "La carpeta de una conversación ya empezada no se puede cambiar.",
                            ),
                            _ => ("Cambiar carpeta…", "Carpeta en la que trabaja Claude."),
                        };
                        if ui.button(label).on_hover_text(tip).clicked() {
                            actions.push(Action::PickFolder(target));
                            ui.close();
                        }
                    })
                    .on_hover_text(path.clone());

                    // Worktree branch of the conversation
                    if let Some(wt) = &worktree {
                        Frame::new()
                            .fill(theme::tint(OK, 38))
                            .corner_radius(R_CHIP)
                            .inner_margin(Margin::symmetric(9, 2))
                            .show(ui, |ui| {
                                ui.label(RichText::new(trunc(&wt.branch, 34)).small().color(OK));
                            })
                            .response
                            .on_hover_text(wt.path.display().to_string());
                    }

                    // Mode
                    let mut mode = live_mode.unwrap_or_else(|| app.settings.mode.clone());
                    let before = mode.clone();
                    let mode_color = match mode.as_str() {
                        "plan" => AMBER,
                        "bypassPermissions" => RED,
                        _ => TEXT,
                    };
                    let mut mode_tip = "Modo de permisos".to_owned();
                    if live.is_none() {
                        mode_tip = format!("{mode_tip}. {next_conv}");
                    }
                    chip(ui, mode_short(&mode), mode_color, |ui| {
                        for (v, label) in MODES {
                            if ui.selectable_label(mode == v, label).clicked() {
                                mode = v.to_owned();
                                ui.close();
                            }
                        }
                    })
                    .on_hover_text(mode_tip);
                    if mode != before {
                        app.settings.mode = mode.clone();
                        actions.push(Action::SetMode(mode));
                    }

                    // Model
                    let mut model = app.settings.model.clone();
                    let mut model_tip = match real_model.as_deref() {
                        Some(r) => format!("Modelo: {r}"),
                        None => "Modelo".to_owned(),
                    };
                    if live.is_none() {
                        model_tip = format!("{model_tip}. {next_conv}");
                    }
                    chip(ui, &model.clone(), TEXT, |ui| {
                        for m in MODELS {
                            if ui.selectable_label(model == m, m).clicked() {
                                model = m.to_owned();
                                ui.close();
                            }
                        }
                    })
                    .on_hover_text(model_tip);
                    if model != app.settings.model {
                        app.settings.model = model.clone();
                        actions.push(Action::SetModel(model));
                    }

                    // Center: conversation title + state chip
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        let Some(t) = conv_title else { return };
                        let t = trunc(&t, 56);
                        let font = egui::FontId::new(15.0, theme::semibold_family());
                        let tw = ui.painter().layout_no_wrap(t.clone(), font, TEXT).size().x;
                        let chip_w = if status.is_some() { 150.0 } else { 0.0 };
                        ui.add_space(((ui.available_width() - tw - chip_w) / 2.0).max(8.0));
                        ui.label(theme::semibold(t).size(15.0));
                        if let Some(st) = &status {
                            let c = theme::status_color(st);
                            Frame::new()
                                .fill(theme::tint(c, 38))
                                .corner_radius(R_CHIP)
                                .inner_margin(Margin::symmetric(9, 2))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 4.0;
                                        dot(ui, c, 4.0);
                                        ui.label(
                                            RichText::new(theme::status_label(st)).small().color(c),
                                        );
                                    });
                                });
                        }
                    });
                });
            });
        });

    if let Some(e) = app.error.clone() {
        egui::Panel::top("error_strip")
            .frame(
                Frame::new()
                    .fill(theme::tint(RED, 40))
                    .inner_margin(Margin::symmetric(14, 6)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(e).color(RED));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(RichText::new("Cerrar").color(MUTED)).clicked() {
                            app.error = None;
                        }
                    });
                });
            });
    }
}
