//! Left panel: session groups and history.

use chrono::{DateTime, Local};
use egui::{Align2, FontId, Frame, Id, Margin, RichText, ScrollArea, Sense, Ui, UiBuilder, vec2};

use super::theme::{self, ACCENT, BG, LINE, MUTED, RAISED, SURFACE, TEXT};
use super::{Action, DeckApp, Selected, short_dir, trunc};
use crate::history::{HistoryEntry, is_test_cwd};
use crate::session::SessionKey;
use crate::state::{Session, SessionStatus};

/// Id of the history filter field (Ctrl+F focuses it).
pub fn filter_id() -> Id {
    Id::NULL.with("history_filter")
}

pub fn show(ui: &mut Ui, app: &mut DeckApp, actions: &mut Vec<Action>) {
    egui::Panel::left("sidebar")
        .resizable(true)
        .default_size(290.0)
        .size_range(220.0..=520.0)
        .frame(
            Frame::new()
                .fill(SURFACE)
                .stroke(egui::Stroke::new(1.0, LINE))
                .inner_margin(Margin::same(10)),
        )
        .show(ui, |ui| {
            let primary = egui::Button::new(RichText::new("Nueva conversación").color(BG))
                .fill(ACCENT)
                .stroke(egui::Stroke::NONE);
            if ui
                .add_sized([ui.available_width(), 32.0], primary)
                .on_hover_text("Ctrl+N")
                .clicked()
            {
                actions.push(Action::NewConversation);
            }
            ui.add_space(8.0);

            let active = app.active_key();
            let total = app.state.sessions().len();
            if total == 0 {
                ui.label(theme::muted(
                    "Aún no hay conversaciones abiertas. Empieza una nueva o retoma una del historial.",
                ));
            } else {
                let max_h = ui.available_height() * 0.45;
                ScrollArea::vertical()
                    .id_salt("sessions_scroll")
                    .max_height(max_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let g = app.state.grouped();
                        group(ui, "Corriendo", &g.running, active, actions);
                        group(ui, "Pendientes", &g.pending, active, actions);
                        group(ui, "Terminadas", &g.finished, active, actions);
                    });
            }
            ui.add_space(6.0);
            ui.separator();
            history(ui, app, actions);
        });
}

fn group(
    ui: &mut Ui,
    name: &str,
    list: &[&Session],
    active: Option<SessionKey>,
    actions: &mut Vec<Action>,
) {
    if list.is_empty() {
        return;
    }
    ui.add_space(2.0);
    ui.label(theme::muted(format!("{name} · {}", list.len())));
    for s in list {
        row(ui, s, active == Some(s.key), actions);
    }
}

fn session_menu(ui: &mut Ui, s: &Session, actions: &mut Vec<Action>) {
    let alive = !matches!(s.status, SessionStatus::Finished { .. });
    if ui
        .add_enabled(alive, egui::Button::new("Detener"))
        .clicked()
    {
        actions.push(Action::Close(s.key));
        ui.close();
    }
    if ui.button("Quitar de la lista").clicked() {
        actions.push(Action::Delete(s.key));
        ui.close();
    }
}

fn row(ui: &mut Ui, s: &Session, selected: bool, actions: &mut Vec<Action>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
    let hovered = ui.rect_contains_pointer(rect);
    let painter = ui.painter();
    if selected || hovered {
        painter.rect_filled(
            rect,
            6,
            if selected {
                RAISED
            } else {
                theme::tint(RAISED, 140)
            },
        );
    }
    let color = theme::status_color(&s.status);
    if selected {
        let bar =
            egui::Rect::from_min_size(rect.min + vec2(0.0, 6.0), vec2(3.0, rect.height() - 12.0));
        painter.rect_filled(bar, 2, ACCENT);
    }
    let ind = egui::Rect::from_center_size(rect.left_center() + vec2(18.0, 0.0), vec2(16.0, 16.0));
    match s.status {
        SessionStatus::Starting | SessionStatus::Running => {
            ui.put(ind, egui::Spinner::new().size(14.0).color(color));
        }
        _ => {
            ui.painter().circle_filled(ind.center(), 5.0, color);
        }
    }
    let title = if s.title.is_empty() {
        "Nueva conversación".to_owned()
    } else {
        trunc(&s.title, 40)
    };
    let sub = format!("{} · {}", short_dir(&s.cwd), s.updated_at.format("%H:%M"));
    let clip = egui::Rect::from_min_max(
        rect.min + vec2(34.0, 0.0),
        rect.max - vec2(if hovered { 30.0 } else { 6.0 }, 0.0),
    );
    let p = ui.painter().with_clip_rect(clip);
    p.text(
        rect.min + vec2(34.0, 8.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(14.5),
        TEXT,
    );
    p.text(
        rect.min + vec2(34.0, 27.0),
        Align2::LEFT_TOP,
        sub,
        FontId::proportional(12.0),
        MUTED,
    );
    if hovered {
        let btn =
            egui::Rect::from_center_size(rect.right_center() - vec2(16.0, 0.0), vec2(24.0, 24.0));
        ui.scope_builder(UiBuilder::new().max_rect(btn), |ui| {
            ui.menu_button(RichText::new("...").color(MUTED), |ui| {
                session_menu(ui, s, actions)
            });
        });
    }
    if resp.clicked() {
        actions.push(Action::Select(s.key));
    }
    resp.context_menu(|ui| session_menu(ui, s, actions));
}

/// Day bucket for the history list.
fn bucket(dt: DateTime<Local>) -> &'static str {
    let today = Local::now().date_naive();
    let d = dt.date_naive();
    let days = (today - d).num_days();
    if days <= 0 {
        "Hoy"
    } else if days == 1 {
        "Ayer"
    } else if days < 7 {
        "Esta semana"
    } else {
        "Anteriores"
    }
}

fn history_row(ui: &mut Ui, e: &HistoryEntry, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    if selected || resp.hovered() {
        ui.painter().rect_filled(
            rect,
            6,
            if selected {
                RAISED
            } else {
                theme::tint(RAISED, 140)
            },
        );
    }
    if selected {
        let bar =
            egui::Rect::from_min_size(rect.min + vec2(0.0, 6.0), vec2(3.0, rect.height() - 12.0));
        ui.painter().rect_filled(bar, 2, ACCENT);
    }
    let p = ui.painter().with_clip_rect(rect.shrink2(vec2(6.0, 0.0)));
    p.text(
        rect.min + vec2(12.0, 6.0),
        Align2::LEFT_TOP,
        trunc(&e.title, 44),
        FontId::proportional(14.0),
        TEXT,
    );
    let sub = format!(
        "{} · {}",
        short_dir(e.cwd.as_deref().unwrap_or("?")),
        e.modified.format("%H:%M")
    );
    p.text(
        rect.min + vec2(12.0, 25.0),
        Align2::LEFT_TOP,
        sub,
        FontId::proportional(12.0),
        MUTED,
    );
    resp
}

fn history(ui: &mut Ui, app: &mut DeckApp, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.label(theme::semibold("Historial"));
        if app.history_busy {
            ui.add(egui::Spinner::new().size(12.0));
        } else if ui
            .small_button("Actualizar")
            .on_hover_text("Volver a leer ~/.claude/projects")
            .clicked()
        {
            actions.push(Action::RefreshHistory);
        }
    });
    ui.add(
        egui::TextEdit::singleline(&mut app.history_filter)
            .id(filter_id())
            .hint_text("Buscar (Ctrl+F)")
            .desired_width(f32::INFINITY),
    );

    // Folder filter: distinct folders of the visible (test-filtered) entries.
    let show_tests = app.settings.show_test_sessions;
    let mut folders: Vec<String> = Vec::new();
    for e in &app.history {
        if let Some(c) = &e.cwd
            && (show_tests || !is_test_cwd(c))
            && !folders.contains(c)
        {
            folders.push(c.clone());
        }
    }
    folders.sort_by_key(|f| short_dir(f).to_lowercase());
    if let Some(sel) = &app.history_folder
        && !folders.contains(sel)
    {
        app.history_folder = None;
    }
    let selected_text = match &app.history_folder {
        Some(f) => short_dir(f),
        None => "Todas las carpetas".to_owned(),
    };
    egui::ComboBox::from_id_salt("history_folder")
        .selected_text(trunc(&selected_text, 30))
        .width(ui.available_width() - 8.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut app.history_folder, None, "Todas las carpetas");
            for f in &folders {
                ui.selectable_value(&mut app.history_folder, Some(f.clone()), short_dir(f))
                    .on_hover_text(f.clone());
            }
        });
    ui.checkbox(
        &mut app.settings.show_test_sessions,
        "Mostrar sesiones de prueba",
    );
    ui.add_space(4.0);

    let f = app.history_filter.to_lowercase();
    let selected_id = match &app.selected {
        Some(Selected::History(h)) => Some(h.entry.session_id.clone()),
        _ => None,
    };
    let open_ids: Vec<&str> = app
        .state
        .sessions()
        .iter()
        .filter_map(|s| s.session_id.as_deref())
        .collect();
    let visible: Vec<&HistoryEntry> = app
        .history
        .iter()
        .filter(|e| !open_ids.contains(&e.session_id.as_str()))
        .filter(|e| show_tests || !e.cwd.as_deref().is_some_and(is_test_cwd))
        .filter(|e| app.history_folder.is_none() || e.cwd == app.history_folder)
        .filter(|e| {
            f.is_empty()
                || e.title.to_lowercase().contains(&f)
                || e.cwd.as_deref().unwrap_or("").to_lowercase().contains(&f)
        })
        .collect();

    ScrollArea::vertical()
        .id_salt("history_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if visible.is_empty() {
                ui.label(theme::muted("Sin resultados"));
            }
            let mut last = "";
            for e in visible {
                let b = bucket(e.modified);
                if b != last {
                    ui.add_space(4.0);
                    ui.label(theme::muted(b));
                    last = b;
                }
                let sel = selected_id.as_deref() == Some(e.session_id.as_str());
                if history_row(ui, e, sel).clicked() {
                    actions.push(Action::OpenHistory(Box::new(e.clone())));
                }
            }
        });
}
