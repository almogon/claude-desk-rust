//! Inline permission card (`can_use_tool`).

use std::collections::HashMap;

use egui::{Frame, Margin, RichText, Stroke, Ui};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use serde_json::Value;

use super::chat::tool_summary;
use super::theme::{self, ACCENT, AMBER, BG, MUTED, OK, R_CARD, RED};
use super::{Action, dot, trunc};
use crate::protocol::PermissionDecision;
use crate::session::SessionKey;
use crate::state::PermissionItem;

/// Tooltip text for an `addRules` suggestion ("Permitir siempre").
fn always_tooltip(rule: &Value) -> String {
    let rules: Vec<String> = rule
        .get("rules")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|r| {
                    let tool = r.get("toolName").and_then(Value::as_str).unwrap_or("?");
                    match r.get("ruleContent").and_then(Value::as_str) {
                        Some(c) => format!("{tool}({c})"),
                        None => tool.to_owned(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let dest = match rule.get("destination").and_then(Value::as_str) {
        Some("localSettings") => "Guarda la regla en .claude/settings.local.json de este proyecto.",
        Some("projectSettings") => "Guarda la regla en .claude/settings.json de este proyecto.",
        Some("userSettings") => "Guarda la regla en ~/.claude/settings.json (todos tus proyectos).",
        Some("session") => "La regla solo vale durante esta sesión.",
        _ => "Guarda la regla que propone Claude Code.",
    };
    if rules.is_empty() {
        dest.to_owned()
    } else {
        format!("Permite {}. {dest}", rules.join(", "))
    }
}

pub fn card(
    ui: &mut Ui,
    p: &PermissionItem,
    key: Option<SessionKey>,
    cache: &mut CommonMarkCache,
    reasons: &mut HashMap<String, String>,
    actions: &mut Vec<Action>,
) {
    if let Some(allowed) = p.resolved {
        let (txt, color) = if allowed {
            (format!("Permitido: {}", p.tool_name), OK)
        } else {
            (format!("Denegado: {}", p.tool_name), RED)
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            dot(ui, color, 3.5);
            ui.label(RichText::new(txt).small().color(MUTED));
        });
        return;
    }
    let Some(key) = key else {
        ui.label(theme::muted(format!("Permiso pendiente: {}", p.tool_name)));
        return;
    };

    let is_plan = p.tool_name == "ExitPlanMode";
    let is_ask = p.tool_name == "AskUserQuestion";
    let deny_id = ui.make_persistent_id(("deny_open", &p.request_id));
    Frame::new()
        .fill(theme::tint(AMBER, 18))
        .stroke(Stroke::new(1.0, AMBER))
        .corner_radius(R_CARD)
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let title = if is_plan {
                "Plan propuesto".to_owned()
            } else if is_ask {
                "Claude te pregunta".to_owned()
            } else {
                format!("Permiso solicitado · {}", p.tool_name)
            };
            ui.label(theme::semibold(title).color(AMBER));
            if let Some(d) = &p.description
                && !is_plan
                && !is_ask
            {
                ui.label(theme::muted(trunc(d, 200)));
            }

            if is_plan {
                let plan = p.input.get("plan").and_then(|v| v.as_str()).unwrap_or("");
                CommonMarkViewer::new().show(ui, cache, plan);
            } else if is_ask {
                ask_body(ui, &p.input);
                ui.label(theme::muted(
                    "Por ahora solo se puede permitir o denegar; la respuesta elegida no se envía.",
                ));
            } else {
                let s = tool_summary(&p.tool_name, &p.input);
                if !s.is_empty() {
                    Frame::new()
                        .fill(BG)
                        .corner_radius(6)
                        .inner_margin(Margin::symmetric(8, 6))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(s).monospace());
                        });
                }
                ui.collapsing("Ver entrada completa", |ui| {
                    let pretty = serde_json::to_string_pretty(&p.input).unwrap_or_default();
                    ui.label(RichText::new(trunc(&pretty, 3000)).monospace().small());
                });
            }

            let (yes, no) = if is_plan {
                ("Aprobar plan", "Seguir planificando…")
            } else {
                ("Permitir", "Denegar…")
            };
            let allow = |updated_permissions: Vec<Value>| Action::Permission {
                key,
                request_id: p.request_id.clone(),
                decision: PermissionDecision::Allow {
                    updated_input: p.input.clone(),
                    updated_permissions,
                },
            };
            let deny_open = ui.data(|d| d.get_temp::<bool>(deny_id)).unwrap_or(false);
            ui.add_space(4.0);
            if !deny_open {
                ui.horizontal_wrapped(|ui| {
                    let primary = egui::Button::new(RichText::new(yes).color(BG))
                        .fill(ACCENT)
                        .stroke(Stroke::NONE);
                    if ui.add(primary).clicked() {
                        actions.push(allow(Vec::new()));
                    }
                    if !is_plan && !is_ask {
                        if let Some(rule) = p.add_rules_suggestion() {
                            if ui
                                .button("Permitir siempre")
                                .on_hover_text(always_tooltip(rule))
                                .clicked()
                            {
                                actions.push(allow(vec![rule.clone()]));
                            }
                        } else if let Some(mode) = p.accept_edits_suggestion()
                            && ui
                                .button("Aceptar ediciones en esta sesión")
                                .on_hover_text(
                                    "Las ediciones siguientes no pedirán permiso hasta que termine la sesión.",
                                )
                                .clicked()
                        {
                            actions.push(allow(vec![mode.clone()]));
                        }
                    }
                    if ui.button(no).clicked() {
                        ui.data_mut(|d| d.insert_temp(deny_id, true));
                    }
                });
            } else {
                let reason = reasons.entry(p.request_id.clone()).or_default();
                ui.add(
                    egui::TextEdit::singleline(reason)
                        .hint_text("Motivo para Claude (opcional)")
                        .desired_width(f32::INFINITY),
                );
                ui.horizontal(|ui| {
                    let confirm =
                        egui::Button::new(RichText::new("Confirmar").color(BG)).fill(RED);
                    if ui.add(confirm).clicked() {
                        let message = if reason.trim().is_empty() {
                            if is_plan {
                                "El usuario quiere seguir planificando; no ejecutes el plan todavía."
                                    .to_owned()
                            } else {
                                "El usuario denegó el permiso.".to_owned()
                            }
                        } else {
                            reason.trim().to_owned()
                        };
                        actions.push(Action::Permission {
                            key,
                            request_id: p.request_id.clone(),
                            decision: PermissionDecision::Deny { message },
                        });
                    }
                    if ui.button("Cancelar").clicked() {
                        ui.data_mut(|d| d.insert_temp(deny_id, false));
                    }
                });
            }
        });
}

/// Readable rendering of `AskUserQuestion` input (`questions[].options[]`).
fn ask_body(ui: &mut Ui, input: &serde_json::Value) {
    let Some(qs) = input.get("questions").and_then(|v| v.as_array()) else {
        ui.label(
            RichText::new(trunc(&input.to_string(), 500))
                .monospace()
                .small(),
        );
        return;
    };
    for q in qs {
        if let Some(h) = q.get("header").and_then(|v| v.as_str()) {
            ui.label(theme::muted(h));
        }
        if let Some(t) = q.get("question").and_then(|v| v.as_str()) {
            ui.label(theme::semibold(t));
        }
        if let Some(opts) = q.get("options").and_then(|v| v.as_array()) {
            for o in opts {
                let label = o.get("label").and_then(|v| v.as_str()).unwrap_or("?");
                let desc = o.get("description").and_then(|v| v.as_str()).unwrap_or("");
                if desc.is_empty() {
                    ui.label(format!("  • {label}"));
                } else {
                    ui.label(format!("  • {label} — {desc}"));
                }
            }
        }
        ui.add_space(4.0);
    }
}
