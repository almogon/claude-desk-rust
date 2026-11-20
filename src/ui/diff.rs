//! Right panel: working-tree changes (git diff against HEAD) of the active folder.

use egui::{Align, Align2, FontId, Frame, Layout, Margin, RichText, ScrollArea, Sense, Ui, vec2};

use super::theme::{self, AMBER, BLUE, LINE, MUTED, OK, RAISED, RED, SURFACE, TEXT};
use super::{Action, DeckApp, DiffOutcome};
use crate::git::{FileDiff, FileStatus, LineKind};

const ROW_H: f32 = 16.0;
/// Characters drawn per diff line (keeps text layout cheap).
const MAX_CHARS: usize = 400;

/// Narrowest width of the panel.
pub const MIN_WIDTH: f32 = 320.0;
const MAX_WIDTH: f32 = 1000.0;

pub fn show(ui: &mut Ui, app: &mut DeckApp, actions: &mut Vec<Action>) {
    if !app.settings.diff_open {
        return;
    }
    let busy = app.diff_busy;
    let max_w = (ui.available_width() - super::MIN_CHAT_WIDTH).clamp(MIN_WIDTH, MAX_WIDTH);
    egui::Panel::right("diff_panel")
        .resizable(true)
        .default_size(460.0)
        .size_range(MIN_WIDTH..=max_w)
        .frame(
            Frame::new()
                .fill(SURFACE)
                .stroke(egui::Stroke::new(1.0, LINE))
                .inner_margin(Margin::same(10)),
        )
        .show(ui, |ui| {
            let count = match app.diff.as_ref().map(|d| &d.outcome) {
                Some(DiffOutcome::Files(f)) => Some(f.len()),
                _ => None,
            };
            ui.horizontal(|ui| {
                ui.label(theme::semibold("Cambios"));
                if let Some(n) = count {
                    ui.label(theme::muted(format!(
                        "· {n} {}",
                        if n == 1 { "archivo" } else { "archivos" }
                    )));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if busy {
                        ui.add(egui::Spinner::new().size(12.0));
                    } else if ui.small_button("Actualizar").clicked() {
                        actions.push(Action::RefreshDiff);
                    }
                });
            });
            ui.separator();

            let Some(view) = app.diff.as_ref() else {
                if !busy {
                    ui.label(theme::muted("Sin datos todavía"));
                }
                return;
            };
            match &view.outcome {
                DiffOutcome::NotRepo => {
                    ui.label(theme::muted("La carpeta no es un repositorio git"));
                }
                DiffOutcome::Error(e) => {
                    ui.label(RichText::new(e).color(RED));
                }
                DiffOutcome::Files(files) if files.is_empty() => {
                    ui.label(theme::muted("Sin cambios"));
                }
                DiffOutcome::Files(files) => {
                    let sel = app
                        .diff_selected
                        .as_ref()
                        .and_then(|p| files.iter().position(|f| &f.path == p))
                        .unwrap_or(0);
                    let mut clicked: Option<usize> = None;
                    ScrollArea::vertical()
                        .id_salt("diff_files")
                        .max_height(ui.available_height() * 0.35)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (i, f) in files.iter().enumerate() {
                                if file_row(ui, f, i == sel).clicked() {
                                    clicked = Some(i);
                                }
                            }
                        });
                    if let Some(i) = clicked {
                        app.diff_selected = Some(files[i].path.clone());
                    }
                    ui.separator();
                    file_body(ui, &files[clicked.unwrap_or(sel)]);
                }
            }
        });
}

fn status_style(s: FileStatus) -> (&'static str, egui::Color32) {
    match s {
        FileStatus::Added => ("A", OK),
        FileStatus::Modified => ("M", AMBER),
        FileStatus::Deleted => ("D", RED),
        FileStatus::Renamed => ("R", BLUE),
        FileStatus::Untracked => ("U", MUTED),
    }
}

fn file_row(ui: &mut Ui, f: &FileDiff, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
    if selected || resp.hovered() {
        ui.painter().rect_filled(
            rect,
            4,
            if selected {
                RAISED
            } else {
                theme::tint(RAISED, 140)
            },
        );
    }
    let (letter, color) = status_style(f.status);
    let mono = FontId::monospace(12.5);
    let p = ui.painter();
    p.text(
        rect.left_center() + vec2(8.0, 0.0),
        Align2::LEFT_CENTER,
        letter,
        mono.clone(),
        color,
    );
    // Counters, right aligned: "+N" then "−M".
    let mut x = rect.right() - 6.0;
    let removed = format!("−{}", f.removed);
    let r = p.text(
        egui::pos2(x, rect.center().y),
        Align2::RIGHT_CENTER,
        removed,
        mono.clone(),
        RED,
    );
    x = r.left() - 8.0;
    let r = p.text(
        egui::pos2(x, rect.center().y),
        Align2::RIGHT_CENTER,
        format!("+{}", f.added),
        mono.clone(),
        OK,
    );
    let clip = egui::Rect::from_min_max(
        rect.min + vec2(26.0, 0.0),
        egui::pos2(r.left() - 8.0, rect.max.y),
    );
    p.with_clip_rect(clip).text(
        rect.left_center() + vec2(26.0, 0.0),
        Align2::LEFT_CENTER,
        &f.path,
        mono,
        TEXT,
    );
    resp.on_hover_text(&f.path)
}

enum Row<'a> {
    Header(&'a str),
    Line(LineKind, &'a str),
}

fn file_body(ui: &mut Ui, f: &FileDiff) {
    if f.binary {
        ui.label(theme::muted("Archivo binario o demasiado grande"));
        return;
    }
    let rows: Vec<Row> = f
        .hunks
        .iter()
        .flat_map(|h| {
            std::iter::once(Row::Header(&h.header))
                .chain(h.lines.iter().map(|l| Row::Line(l.kind, &l.text)))
        })
        .collect();
    if rows.is_empty() {
        ui.label(theme::muted("Sin cambios de contenido"));
        return;
    }
    if f.truncated {
        ui.label(theme::muted(format!(
            "Diff recortado a {} líneas",
            crate::git::MAX_LINES_PER_FILE
        )));
    }
    let font = FontId::monospace(12.0);
    ScrollArea::vertical()
        .id_salt(("diff_body", &f.path))
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for row in &rows[range] {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
                let (bg, color, prefix, text) = match row {
                    Row::Header(h) => (None, MUTED, "", *h),
                    Row::Line(LineKind::Add, t) => (Some(theme::tint(OK, 40)), TEXT, "+ ", *t),
                    Row::Line(LineKind::Del, t) => (Some(theme::tint(RED, 40)), TEXT, "− ", *t),
                    Row::Line(LineKind::Context, t) => (None, TEXT, "  ", *t),
                };
                if let Some(bg) = bg {
                    ui.painter().rect_filled(rect, 0, bg);
                }
                let shown: String = text.replace('\t', "    ").chars().take(MAX_CHARS).collect();
                ui.painter().with_clip_rect(rect).text(
                    rect.left_center() + vec2(4.0, 0.0),
                    Align2::LEFT_CENTER,
                    format!("{prefix}{shown}"),
                    font.clone(),
                    color,
                );
            }
        });
}
