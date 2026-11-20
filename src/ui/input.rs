//! Message input at the bottom of the center area.

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use egui::text::CCursor;
use egui::text_selection::CCursorRange;
use egui::{Event, Frame, Key, Margin, Modifiers, RichText, ScrollArea, Stroke, TextEdit, Ui};

use super::theme::{self, ACCENT, BG, LINE, MUTED, R_CARD, RAISED, RED, SURFACE};
use super::{Action, DeckApp, reading_column};
use crate::protocol::{Attachment, SlashCommand};
use crate::session::SessionKey;
use crate::state::{SessionStatus, filter_commands};

/// Pasted text longer than this becomes an attachment instead of going into the box.
const LONG_PASTE_LINES: usize = 15;
const LONG_PASTE_CHARS: usize = 1500;
/// PNGs bigger than this are downscaled before sending.
const MAX_PNG_BYTES: usize = 5 * 1024 * 1024;
const MAX_SIDE: u32 = 2000;
const THUMB: f32 = 48.0;

/// What the user is composing for one session.
#[derive(Default)]
pub struct Draft {
    pub text: String,
    pub attachments: Vec<Attachment>,
}

/// Encodes an RGBA image as a PNG attachment, downscaling it if the PNG is huge.
fn image_attachment(mut img: image::RgbaImage) -> Option<Attachment> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let encode = |img: &image::RgbaImage| -> Option<Vec<u8>> {
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
            .ok()?;
        Some(buf)
    };
    let mut png = encode(&img)?;
    if png.len() > MAX_PNG_BYTES && img.width().max(img.height()) > MAX_SIDE {
        let (w, h) = (img.width(), img.height());
        let scale = f64::from(MAX_SIDE) / f64::from(w.max(h));
        let nw = ((f64::from(w) * scale).round() as u32).max(1);
        let nh = ((f64::from(h) * scale).round() as u32).max(1);
        img = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3);
        png = encode(&img)?;
    }
    Some(Attachment::Image {
        id: NEXT.fetch_add(1, Ordering::Relaxed),
        png: Arc::from(png),
        w: img.width(),
        h: img.height(),
    })
}

/// True if the path looks like an image we can decode.
fn is_image_path(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp"))
}

/// Adds a file to the draft: images become attachments, anything else an `@path`.
fn add_path(draft: &mut Draft, path: &Path) {
    if is_image_path(path)
        && let Ok(bytes) = std::fs::read(path)
        && let Ok(img) = image::load_from_memory(&bytes)
        && let Some(a) = image_attachment(img.to_rgba8())
    {
        draft.attachments.push(a);
        return;
    }
    let p = path.display().to_string();
    if !draft.text.is_empty() && !draft.text.ends_with(char::is_whitespace) {
        draft.text.push(' ');
    }
    if p.contains(char::is_whitespace) {
        draft.text.push_str(&format!("@\"{p}\" "));
    } else {
        draft.text.push_str(&format!("@{p} "));
    }
}

/// Reads files or an image from the system clipboard into the draft.
/// Returns true if something was added.
fn paste_from_clipboard(draft: &mut Draft) -> bool {
    let Ok(mut cb) = arboard::Clipboard::new() else {
        return false;
    };
    if let Ok(files) = cb.get().file_list()
        && !files.is_empty()
    {
        for f in &files {
            add_path(draft, f);
        }
        return true;
    }
    if let Ok(img) = cb.get_image()
        && let Some(rgba) =
            image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned())
        && let Some(a) = image_attachment(rgba)
    {
        draft.attachments.push(a);
        return true;
    }
    false
}

/// Rising edge of Ctrl+V according to the OS. egui-winit emits no event at all
/// when the clipboard has no text (image / file copies), so poll the key state.
#[cfg(windows)]
fn os_paste_edge(ctx: &egui::Context) -> bool {
    unsafe extern "system" {
        fn GetAsyncKeyState(vkey: i32) -> i16;
    }
    // SAFETY: plain Win32 call with no pointers.
    let down = unsafe { GetAsyncKeyState(0x11) < 0 && GetAsyncKeyState(0x56) < 0 };
    let id = egui::Id::NULL.with("os_paste_prev");
    let prev = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    ctx.data_mut(|d| d.insert_temp(id, down));
    if down {
        // Keep frames coming so the release is noticed.
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
    }
    down && !prev
}

#[cfg(not(windows))]
fn os_paste_edge(_ctx: &egui::Context) -> bool {
    false
}

/// Handles dropped files and pastes for the focused input.
fn collect_incoming(ui: &mut Ui, draft: &mut Draft, focused: bool) {
    let dropped = ui.input(|i| i.raw.dropped_files.clone());
    for f in dropped {
        add_path(draft, f.path());
    }
    if !focused {
        return;
    }
    let edge = os_paste_edge(ui.ctx());
    let key_v = ui.input(|i| i.modifiers.command && i.key_pressed(Key::V));
    let text = ui.input(|i| {
        i.events.iter().find_map(|e| match e {
            Event::Paste(t) => Some(t.clone()),
            _ => None,
        })
    });
    match text {
        Some(t) => {
            let lines = t.lines().count();
            if lines > LONG_PASTE_LINES || t.chars().count() > LONG_PASTE_CHARS {
                draft.attachments.push(Attachment::PastedText {
                    text: t.replace("\r\n", "\n"),
                    lines,
                });
                ui.input_mut(|i| i.events.retain(|e| !matches!(e, Event::Paste(_))));
            }
        }
        None => {
            if (edge || key_v) && paste_from_clipboard(draft) {
                ui.input_mut(|i| i.events.retain(|e| !matches!(e, Event::Paste(_))));
            }
        }
    }
}

/// Row of attachment chips; returns the index to remove, if any.
fn chips_row(ui: &mut Ui, atts: &[Attachment]) -> Option<usize> {
    let mut remove = None;
    ui.horizontal_wrapped(|ui| {
        for (i, a) in atts.iter().enumerate() {
            Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0, LINE))
                .corner_radius(R_CARD)
                .inner_margin(Margin::symmetric(6, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if let Attachment::Image { id, png, .. } = a {
                            ui.add(
                                egui::Image::from_bytes(
                                    format!("bytes://attachment-{id}.png"),
                                    egui::load::Bytes::Shared(png.clone()),
                                )
                                .max_height(THUMB)
                                .max_width(THUMB * 2.0)
                                .corner_radius(4),
                            );
                        }
                        ui.label(RichText::new(a.label()).small().color(MUTED));
                        if ui
                            .small_button("×")
                            .on_hover_text("Remove attachment")
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    });
                });
        }
    });
    remove
}

/// Max rows in the slash-command popup.
const POPUP_MAX: usize = 5;

/// Popup state kept in egui temp memory, keyed by the input.
#[derive(Clone, Default)]
struct PopupState {
    selected: usize,
    dismissed: bool,
    /// Draft text last frame, to detect edits.
    last_text: String,
    /// Popup rectangle last frame.
    rect: Option<egui::Rect>,
}

fn popup_open_id() -> egui::Id {
    egui::Id::NULL.with("slash_cmd_popup_open")
}

/// True if the slash-command popup was open last frame, so global
/// shortcuts (Esc = interrupt) leave its keys alone.
pub fn popup_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(popup_open_id()))
        .unwrap_or(false)
}

/// Draws one popup row; returns true if it was clicked.
fn command_row(ui: &mut Ui, c: &SlashCommand, selected: bool, id: egui::Id) -> bool {
    let fill = if selected {
        ACCENT.gamma_multiply(0.2)
    } else {
        egui::Color32::TRANSPARENT
    };
    let inner = Frame::new()
        .fill(fill)
        .corner_radius(4)
        .inner_margin(Margin::symmetric(6, 3))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let name = RichText::new(format!("/{}", c.name)).strong();
                ui.label(if selected { name.color(ACCENT) } else { name });
                if !c.argument_hint.is_empty() {
                    ui.label(theme::muted(c.argument_hint.as_str()));
                }
                if !c.description.is_empty() {
                    ui.add(egui::Label::new(theme::muted(c.description.as_str())).truncate());
                }
            });
        });
    let resp = ui.interact(inner.response.rect, id, egui::Sense::click());
    resp.clicked()
}

pub fn show(ui: &mut Ui, app: &mut DeckApp, key: SessionKey, actions: &mut Vec<Action>) {
    let Some(s) = app.state.session(key) else {
        return;
    };
    let status = s.status.clone();
    let has_id = s.session_id.is_some();
    let alive = app.is_alive(key);

    egui::Panel::bottom("input_panel")
        .frame(
            Frame::new()
                .fill(BG)
                .inner_margin(Margin::symmetric(14, 10)),
        )
        .show(ui, |ui| {
            if !alive || matches!(status, SessionStatus::Finished { .. }) {
                reading_column(ui, |ui| {
                    Frame::new()
                        .fill(SURFACE)
                        .stroke(Stroke::new(1.0, LINE))
                        .corner_radius(R_CARD)
                        .inner_margin(Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(theme::muted("The conversation has ended."));
                                let btn = egui::Button::new(
                                    RichText::new("Resume conversation").color(BG),
                                )
                                .fill(ACCENT)
                                .stroke(Stroke::NONE);
                                if ui
                                    .add_enabled(has_id, btn)
                                    .on_disabled_hover_text("No session ID yet")
                                    .clicked()
                                {
                                    actions.push(Action::ResumeSession(key));
                                }
                            });
                        });
                });
                return;
            }

            let id = ui.make_persistent_id(("input", key.0));
            let focused = ui.memory(|m| m.has_focus(id));
            // Slash-command popup: keys are consumed here, before the TextEdit.
            let popup_id = ui.make_persistent_id(("cmd_popup", key.0));
            let mut pstate: PopupState = ui.data(|d| d.get_temp(popup_id)).unwrap_or_default();
            let text_now = app
                .drafts
                .get(&key)
                .map(|d| d.text.clone())
                .unwrap_or_default();
            if pstate.last_text != text_now {
                pstate.last_text.clone_from(&text_now);
                pstate.selected = 0;
                pstate.dismissed = false;
            }
            // A click on the popup first steals focus from the input, so keep
            // the popup alive while the pointer is over it.
            let pointer_in_popup = pstate.rect.is_some_and(|r| {
                ui.input(|i| i.pointer.hover_pos())
                    .is_some_and(|p| r.contains(p))
            });
            let matches: Vec<SlashCommand> = if (focused || pointer_in_popup)
                && !pstate.dismissed
                && text_now.starts_with('/')
                && !text_now.chars().any(char::is_whitespace)
            {
                app.state
                    .session(key)
                    .map(|s| {
                        filter_commands(&s.commands, &text_now[1..], POPUP_MAX)
                            .into_iter()
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let popup_visible = !matches.is_empty();
            if popup_visible {
                pstate.selected = pstate.selected.min(matches.len() - 1);
            }
            let mut chosen: Option<String> = None;
            if popup_visible && focused {
                let n = matches.len();
                ui.input_mut(|i| {
                    if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                        pstate.selected = (pstate.selected + 1) % n;
                    }
                    if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                        pstate.selected = (pstate.selected + n - 1) % n;
                    }
                    if i.consume_key(Modifiers::NONE, Key::Escape) {
                        pstate.dismissed = true;
                    }
                    let tab = i.consume_key(Modifiers::NONE, Key::Tab);
                    let enter = !i.modifiers.shift && i.consume_key(Modifiers::NONE, Key::Enter);
                    if tab || enter {
                        chosen = Some(matches[pstate.selected].name.clone());
                    }
                });
            }
            // Enter sends; Shift+Enter falls through to the text edit (newline).
            let enter = focused
                && !popup_visible
                && ui.input_mut(|i| {
                    !i.modifiers.shift && i.consume_key(Modifiers::NONE, Key::Enter)
                });
            if popup_visible && !pstate.dismissed {
                reading_column(ui, |ui| {
                    let r = Frame::new()
                        .fill(SURFACE)
                        .stroke(Stroke::new(1.0, LINE))
                        .corner_radius(R_CARD)
                        .inner_margin(Margin::symmetric(6, 4))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            for (i, c) in matches.iter().enumerate() {
                                if command_row(ui, c, i == pstate.selected, popup_id.with(i)) {
                                    chosen = Some(c.name.clone());
                                }
                            }
                        });
                    pstate.rect = Some(r.response.rect);
                    ui.add_space(4.0);
                });
            } else {
                pstate.rect = None;
            }
            let mut draft_text_changed = false;
            if let Some(name) = chosen {
                let text = format!("/{name} ");
                let len = text.chars().count();
                app.drafts.entry(key).or_default().text = text;
                let ctx = ui.ctx().clone();
                let mut st = TextEdit::load_state(&ctx, id).unwrap_or_default();
                st.cursor
                    .set_char_range(Some(CCursorRange::one(CCursor::new(len))));
                st.store(&ctx, id);
                ui.memory_mut(|m| m.request_focus(id));
                draft_text_changed = true;
            }
            if draft_text_changed {
                pstate.last_text = app
                    .drafts
                    .get(&key)
                    .map(|d| d.text.clone())
                    .unwrap_or_default();
                pstate.selected = 0;
                pstate.dismissed = false;
            }
            let open = popup_visible && !pstate.dismissed;
            ui.data_mut(|d| {
                d.insert_temp(popup_id, pstate);
                d.insert_temp(popup_open_id(), open);
            });
            let draft = app.drafts.entry(key).or_default();
            collect_incoming(ui, draft, focused);
            let mut send = enter;
            reading_column(ui, |ui| {
                Frame::new()
                    .fill(RAISED)
                    .stroke(Stroke::new(1.0, if focused { ACCENT } else { LINE }))
                    .corner_radius(R_CARD)
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if !draft.attachments.is_empty() {
                            if let Some(i) = chips_row(ui, &draft.attachments) {
                                draft.attachments.remove(i);
                            }
                            ui.add_space(4.0);
                        }
                        ui.horizontal_top(|ui| {
                            let btn_w = 92.0;
                            let edit_w = (ui.available_width() - btn_w - 8.0).max(80.0);
                            let line_h = ui.text_style_height(&egui::TextStyle::Body);
                            ScrollArea::vertical()
                                .id_salt(("input_scroll", key.0))
                                .max_height(line_h * 8.0 + 8.0)
                                .max_width(edit_w)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    ui.add(
                                        TextEdit::multiline(&mut draft.text)
                                            .id(id)
                                            .frame(Frame::NONE)
                                            .desired_rows(2)
                                            .desired_width(ui.available_width())
                                            .hint_text("Message to Claude"),
                                    );
                                });
                            let has_content =
                                !draft.text.trim().is_empty() || !draft.attachments.is_empty();
                            let stop = status == SessionStatus::Running && !has_content;
                            if stop {
                                let b = egui::Button::new(RichText::new("Stop").color(RED))
                                    .stroke(Stroke::new(1.0, RED));
                                if ui
                                    .add_sized([btn_w, 28.0], b)
                                    .on_hover_text("Esc")
                                    .clicked()
                                {
                                    actions.push(Action::Interrupt(key));
                                }
                            } else {
                                let b = egui::Button::new(RichText::new("Send").color(BG))
                                    .fill(ACCENT)
                                    .stroke(Stroke::NONE);
                                if ui
                                    .add_enabled_ui(has_content, |ui| {
                                        ui.add_sized([btn_w, 28.0], b)
                                    })
                                    .inner
                                    .on_hover_text("Enter sends · Shift+Enter new line")
                                    .clicked()
                                {
                                    send = true;
                                }
                            }
                        });
                    });
            });
            if send && (!draft.text.trim().is_empty() || !draft.attachments.is_empty()) {
                let text = draft.text.trim().to_owned();
                let atts = std::mem::take(&mut draft.attachments);
                actions.push(Action::Send(key, text, atts));
                draft.text.clear();
                ui.memory_mut(|m| m.request_focus(id));
            }
        });
}
