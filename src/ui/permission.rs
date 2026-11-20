//! Inline permission card (`can_use_tool`).

use std::collections::HashMap;

use egui::{Frame, Margin, RichText, Stroke, Ui};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use serde_json::Value;

use super::chat::tool_summary;
use super::theme::{self, ACCENT, AMBER, BG, MUTED, OK, R_CARD, RED};
use super::{Action, dot, trunc};
use crate::protocol::{PermissionDecision, PermissionMode};
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
        Some("localSettings") => "Saves the rule to .claude/settings.local.json in this project.",
        Some("projectSettings") => "Saves the rule to .claude/settings.json in this project.",
        Some("userSettings") => "Saves the rule to ~/.claude/settings.json (all your projects).",
        Some("session") => "The rule only applies during this session.",
        _ => "Saves the rule Claude Code suggests.",
    };
    if rules.is_empty() {
        dest.to_owned()
    } else {
        format!("Allows {}. {dest}", rules.join(", "))
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
            (format!("Allowed: {}", p.tool_name), OK)
        } else {
            (format!("Denied: {}", p.tool_name), RED)
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            dot(ui, color, 3.5);
            ui.label(RichText::new(txt).small().color(MUTED));
        });
        return;
    }
    let Some(key) = key else {
        ui.label(theme::muted(format!("Permission pending: {}", p.tool_name)));
        return;
    };

    let is_plan = p.tool_name == "ExitPlanMode";
    let is_ask = p.tool_name == "AskUserQuestion";
    let ui_id = ui.make_persistent_id(("perm_ui", &p.request_id));
    let mut st: CardUi = ui.data(|d| d.get_temp(ui_id)).unwrap_or_default();
    let plan_id = ui.make_persistent_id(("plan_edit", &p.request_id));
    let mut plan_st: PlanEdit = ui.data(|d| d.get_temp(plan_id)).unwrap_or_default();
    Frame::new()
        .fill(theme::tint(AMBER, 18))
        .stroke(Stroke::new(1.0, AMBER))
        .corner_radius(R_CARD)
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let title = if is_plan {
                "Proposed plan".to_owned()
            } else if is_ask {
                "Claude is asking you".to_owned()
            } else {
                format!("Permission requested · {}", p.tool_name)
            };
            ui.label(theme::semibold(title).color(AMBER));
            if let Some(d) = &p.description
                && !is_plan
                && !is_ask
            {
                ui.label(theme::muted(trunc(d, 200)));
            }

            // Answers input for AskUserQuestion; `Some` once every question has an answer.
            let mut ask_input: Option<Value> = None;
            let plan = p.input.get("plan").and_then(|v| v.as_str()).unwrap_or("");
            if is_plan {
                if plan_st.editing {
                    ui.add(
                        egui::TextEdit::multiline(&mut plan_st.text)
                            .font(egui::TextStyle::Monospace)
                            .desired_rows(10)
                            .desired_width(f32::INFINITY),
                    );
                } else {
                    CommonMarkViewer::new().show(ui, cache, plan);
                }
            } else if is_ask {
                ask_input = ask_form(ui, &p.request_id, &p.input);
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
                ui.collapsing("Show full input", |ui| {
                    let pretty = serde_json::to_string_pretty(&p.input).unwrap_or_default();
                    ui.label(RichText::new(trunc(&pretty, 3000)).monospace().small());
                });
            }

            ui.add_space(4.0);
            let primary = |label: &str| {
                egui::Button::new(RichText::new(label).color(BG))
                    .fill(ACCENT)
                    .stroke(Stroke::NONE)
            };

            if is_ask {
                if !st.deny_open {
                    ui.horizontal_wrapped(|ui| {
                        let ready = ask_input.is_some();
                        if ui
                            .add_enabled(ready, primary("Send answers"))
                            .on_disabled_hover_text("Answer all the questions.")
                            .clicked()
                            && let Some(updated_input) = ask_input.clone()
                        {
                            actions.push(Action::Permission {
                                key,
                                request_id: p.request_id.clone(),
                                decision: PermissionDecision::Allow {
                                    updated_input,
                                    updated_permissions: Vec::new(),
                                },
                                note: None,
                            });
                        }
                        if ui.button("Deny…").clicked() {
                            st.deny_open = true;
                        }
                    });
                }
            } else {
                let choices = if is_plan {
                    plan_choices()
                } else {
                    tool_choices(p)
                };
                let deny_idx = choices.len() - 1;
                st.selected = st.selected.min(deny_idx);

                // Only the first pending card drawn in a pass owns the keyboard.
                let ctx = ui.ctx().clone();
                let owner_id = egui::Id::NULL.with("perm_key_owner");
                let pass = ctx.cumulative_pass_nr();
                let (owner_pass, owner_rid): (u64, String) =
                    ctx.data(|d| d.get_temp(owner_id)).unwrap_or_default();
                let owns = if owner_pass == pass && owner_pass != 0 {
                    owner_rid == p.request_id
                } else {
                    ctx.data_mut(|d| d.insert_temp(owner_id, (pass, p.request_id.clone())));
                    true
                };
                let note_id = ui.make_persistent_id(("perm_note", &p.request_id));
                let focused = ui.memory(|m| m.focused());
                let can_keys = owns && !st.deny_open && (focused.is_none() || focused == Some(note_id));

                let mut confirm: Option<usize> = None;
                if can_keys {
                    ui.input_mut(|i| {
                        use egui::{Key, Modifiers};
                        let is_sel_allow = !matches!(choices[st.selected].kind, ChoiceKind::Deny);
                        if st.note_open {
                            if i.consume_key(Modifiers::NONE, Key::Escape)
                                || i.consume_key(Modifiers::NONE, Key::Tab)
                            {
                                st.note_open = false;
                            } else if i.consume_key(Modifiers::NONE, Key::Enter) {
                                confirm = Some(st.selected);
                            }
                            return;
                        }
                        if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                            st.selected = move_sel(st.selected, -1, choices.len());
                        }
                        if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                            st.selected = move_sel(st.selected, 1, choices.len());
                        }
                        const DIGITS: [Key; 9] = [
                            Key::Num1,
                            Key::Num2,
                            Key::Num3,
                            Key::Num4,
                            Key::Num5,
                            Key::Num6,
                            Key::Num7,
                            Key::Num8,
                            Key::Num9,
                        ];
                        for (n, k) in DIGITS.into_iter().enumerate() {
                            if i.consume_key(Modifiers::NONE, k)
                                && let Some(idx) = digit_index(n + 1, choices.len())
                            {
                                st.selected = idx;
                                confirm = Some(idx);
                            }
                        }
                        if i.consume_key(Modifiers::NONE, Key::Enter) {
                            confirm = Some(st.selected);
                        }
                        if i.consume_key(Modifiers::NONE, Key::Escape) {
                            st.selected = deny_idx;
                            st.deny_open = true;
                        }
                        if is_sel_allow && i.consume_key(Modifiers::NONE, Key::Tab) {
                            st.note_open = true;
                            st.focus_note = true;
                        }
                    });
                }

                for (i, c) in choices.iter().enumerate() {
                    let is_sel = st.selected == i;
                    let text = RichText::new(format!("{}. {}", i + 1, c.label));
                    let text = if is_sel { text.color(ACCENT) } else { text };
                    let mut r = ui.selectable_label(is_sel, text);
                    if let Some(t) = &c.tooltip {
                        r = r.on_hover_text(t);
                    }
                    if r.clicked() {
                        confirm = Some(i);
                        // The note only applies to the row it was opened on.
                        if st.selected != i {
                            st.note_open = false;
                        }
                        st.selected = i;
                    }
                    let is_allow = !matches!(c.kind, ChoiceKind::Deny);
                    if is_sel && is_allow {
                        ui.horizontal(|ui| {
                            ui.add_space(18.0);
                            if st.note_open {
                                let r = ui.add(
                                    egui::TextEdit::singleline(&mut st.note)
                                        .id(note_id)
                                        .hint_text("Additional instructions for Claude")
                                        .desired_width(f32::INFINITY),
                                );
                                if st.focus_note {
                                    r.request_focus();
                                    st.focus_note = false;
                                }
                            } else {
                                let hint = egui::Label::new(
                                    RichText::new("Tab to add instructions")
                                        .small()
                                        .color(MUTED),
                                )
                                .sense(egui::Sense::click());
                                if ui.add(hint).clicked() {
                                    st.note_open = true;
                                    st.focus_note = true;
                                }
                            }
                        });
                    }
                }

                if let Some(idx) = confirm {
                    let c = &choices[idx];
                    let note = (st.note_open && st.selected == idx).then(|| st.note.clone());
                    match &c.kind {
                        ChoiceKind::Allow(perms) => actions.push(Action::Permission {
                            key,
                            request_id: p.request_id.clone(),
                            decision: PermissionDecision::Allow {
                                updated_input: p.input.clone(),
                                updated_permissions: perms.clone(),
                            },
                            note,
                        }),
                        ChoiceKind::Plan(mode) => actions.push(Action::ApprovePlan {
                            key,
                            request_id: p.request_id.clone(),
                            updated_input: plan_updated_input(
                                &p.input,
                                plan_st.editing.then_some(plan_st.text.as_str()),
                            ),
                            mode: *mode,
                            note,
                        }),
                        ChoiceKind::Deny => {
                            st.selected = idx;
                            st.deny_open = true;
                        }
                    }
                }

                if is_plan {
                    let edit_label = if plan_st.editing {
                        "Discard edit"
                    } else {
                        "Edit plan"
                    };
                    if ui.button(edit_label).clicked() {
                        if plan_st.editing {
                            plan_st = PlanEdit::default();
                        } else {
                            plan_st.editing = true;
                            plan_st.text = plan.to_owned();
                        }
                    }
                }
            }

            if st.deny_open {
                let reason = reasons.entry(p.request_id.clone()).or_default();
                ui.add(
                    egui::TextEdit::singleline(reason)
                        .hint_text("Reason for Claude (optional)")
                        .desired_width(f32::INFINITY),
                );
                ui.horizontal(|ui| {
                    let confirm =
                        egui::Button::new(RichText::new("Confirm").color(BG)).fill(RED);
                    if ui.add(confirm).clicked() {
                        let message = if reason.trim().is_empty() {
                            if is_plan {
                                "The user wants to keep planning; do not execute the plan yet."
                                    .to_owned()
                            } else {
                                "The user denied permission.".to_owned()
                            }
                        } else {
                            reason.trim().to_owned()
                        };
                        actions.push(Action::Permission {
                            key,
                            request_id: p.request_id.clone(),
                            decision: PermissionDecision::Deny { message },
                            note: None,
                        });
                    }
                    if ui.button("Cancel").clicked() {
                        st.deny_open = false;
                    }
                });
            }
        });
    ui.data_mut(|d| {
        d.insert_temp(plan_id, plan_st);
        d.insert_temp(ui_id, st);
    });
}

/// One numbered option of the permission card.
#[derive(Debug, Clone, PartialEq)]
struct Choice {
    label: String,
    tooltip: Option<String>,
    kind: ChoiceKind,
}

#[derive(Debug, Clone, PartialEq)]
enum ChoiceKind {
    /// Allow, with these `updated_permissions`.
    Allow(Vec<Value>),
    /// Approve the plan and switch to this mode.
    Plan(PermissionMode),
    Deny,
}

fn choice(label: &str, tooltip: Option<String>, kind: ChoiceKind) -> Choice {
    Choice {
        label: label.to_owned(),
        tooltip,
        kind,
    }
}

/// Options of a regular tool permission card.
fn tool_choices(p: &PermissionItem) -> Vec<Choice> {
    let mut out = vec![choice("Yes", None, ChoiceKind::Allow(Vec::new()))];
    if let Some(rule) = p.add_rules_suggestion() {
        out.push(choice(
            "Yes, and don't ask again",
            Some(always_tooltip(rule)),
            ChoiceKind::Allow(vec![rule.clone()]),
        ));
    } else if let Some(mode) = p.accept_edits_suggestion() {
        out.push(choice(
            "Yes, and accept edits this session",
            Some(
                "Following edits won't ask for permission until the session ends."
                    .to_owned(),
            ),
            ChoiceKind::Allow(vec![mode.clone()]),
        ));
    }
    out.push(choice(
        "No, and tell Claude what to do differently",
        None,
        ChoiceKind::Deny,
    ));
    out
}

/// Options of the `ExitPlanMode` card.
fn plan_choices() -> Vec<Choice> {
    vec![
        choice(
            "Yes, and auto mode",
            None,
            ChoiceKind::Plan(PermissionMode::Auto),
        ),
        choice(
            "Yes, and accept edits",
            None,
            ChoiceKind::Plan(PermissionMode::AcceptEdits),
        ),
        choice(
            "Yes, and ask for each edit",
            None,
            ChoiceKind::Plan(PermissionMode::Default),
        ),
        choice("No, keep planning", None, ChoiceKind::Deny),
    ]
}

/// Moves the selection by `delta`, clamped to `0..len`.
fn move_sel(sel: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    (sel as i64 + delta as i64).clamp(0, len as i64 - 1) as usize
}

/// Maps a typed digit (1-based) to an option index; out of range is ignored.
fn digit_index(digit: usize, len: usize) -> Option<usize> {
    (digit >= 1 && digit <= len).then(|| digit - 1)
}

/// Selection state of the permission card (egui temp memory, keyed by request id).
#[derive(Clone, Default)]
struct CardUi {
    selected: usize,
    note_open: bool,
    focus_note: bool,
    note: String,
    deny_open: bool,
}

/// Edit state of the plan card (egui temp memory, keyed by request id).
#[derive(Clone, Default)]
struct PlanEdit {
    editing: bool,
    text: String,
}

/// `ExitPlanMode` input to send on approval: the original input, with `plan`
/// replaced by `edited` when the user edited it.
fn plan_updated_input(input: &Value, edited: Option<&str>) -> Value {
    let mut out = input.clone();
    if let (Some(text), Some(obj)) = (edited, out.as_object_mut()) {
        obj.insert("plan".to_owned(), Value::String(text.to_owned()));
    }
    out
}

/// Form state of one `AskUserQuestion` question.
#[derive(Clone, Default)]
struct QuestionAnswer {
    /// One flag per option, in option order.
    selected: Vec<bool>,
    /// "Otra" chosen; its text is in `other_text`.
    other_on: bool,
    other_text: String,
}

fn option_labels(q: &Value) -> Vec<&str> {
    q.get("options")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|o| o.get("label").and_then(Value::as_str).unwrap_or("?"))
                .collect()
        })
        .unwrap_or_default()
}

/// Answer string of one question: selected labels (option order) plus the
/// "Otra" text, joined with ", ". `None` while nothing is answered.
fn answer_for(q: &Value, a: &QuestionAnswer) -> Option<String> {
    let mut parts: Vec<String> = option_labels(q)
        .into_iter()
        .zip(a.selected.iter())
        .filter(|(_, on)| **on)
        .map(|(l, _)| l.to_owned())
        .collect();
    let other = a.other_text.trim();
    if a.other_on && !other.is_empty() {
        parts.push(other.to_owned());
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// `AskUserQuestion` input to send: the original input plus `answers`
/// ({question text -> answer}). `None` unless every question is answered.
fn build_ask_input(input: &Value, answers: &[QuestionAnswer]) -> Option<Value> {
    let qs = input.get("questions").and_then(Value::as_array)?;
    if qs.is_empty() {
        return None;
    }
    let mut map = serde_json::Map::new();
    for (i, q) in qs.iter().enumerate() {
        let text = q.get("question").and_then(Value::as_str)?;
        let ans = answer_for(q, answers.get(i)?)?;
        map.insert(text.to_owned(), Value::String(ans));
    }
    let mut out = input.clone();
    out.as_object_mut()?
        .insert("answers".to_owned(), Value::Object(map));
    Some(out)
}

/// Interactive `AskUserQuestion` form. Returns the input to send once every
/// question has an answer.
fn ask_form(ui: &mut Ui, request_id: &str, input: &Value) -> Option<Value> {
    let Some(qs) = input.get("questions").and_then(Value::as_array) else {
        ui.label(
            RichText::new(trunc(&input.to_string(), 500))
                .monospace()
                .small(),
        );
        return None;
    };
    let state_id = ui.make_persistent_id(("ask_answers", request_id));
    let mut answers: Vec<QuestionAnswer> = ui.data(|d| d.get_temp(state_id)).unwrap_or_default();
    answers.resize_with(qs.len(), QuestionAnswer::default);

    for (qi, q) in qs.iter().enumerate() {
        let a = &mut answers[qi];
        let multi = q
            .get("multiSelect")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let opts = q
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        a.selected.resize(opts.len(), false);
        if let Some(h) = q.get("header").and_then(Value::as_str) {
            ui.label(theme::muted(h));
        }
        if let Some(t) = q.get("question").and_then(Value::as_str) {
            ui.label(theme::semibold(t));
        }
        for (oi, o) in opts.iter().enumerate() {
            let label = o.get("label").and_then(Value::as_str).unwrap_or("?");
            if multi {
                ui.checkbox(&mut a.selected[oi], label);
            } else if ui.radio(a.selected[oi], label).clicked() {
                a.selected.iter_mut().for_each(|s| *s = false);
                a.selected[oi] = true;
                a.other_on = false;
            }
            if let Some(d) = o.get("description").and_then(Value::as_str)
                && !d.is_empty()
            {
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.label(theme::muted(d));
                });
            }
        }
        ui.horizontal(|ui| {
            if multi {
                ui.checkbox(&mut a.other_on, "Other");
            } else if ui.radio(a.other_on, "Other").clicked() {
                a.selected.iter_mut().for_each(|s| *s = false);
                a.other_on = true;
            }
            let r = ui.add(
                egui::TextEdit::singleline(&mut a.other_text)
                    .hint_text("Type your answer")
                    .desired_width(f32::INFINITY),
            );
            if r.changed() && !a.other_text.is_empty() {
                a.other_on = true;
                if !multi {
                    a.selected.iter_mut().for_each(|s| *s = false);
                }
            }
        });
        ui.add_space(4.0);
    }

    let result = build_ask_input(input, &answers);
    ui.data_mut(|d| d.insert_temp(state_id, answers));
    result
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn input() -> Value {
        json!({"questions":[
            {"question":"Color?","header":"Color","multiSelect":false,
             "options":[{"label":"Rojo","description":"r"},{"label":"Azul","description":"a"}]},
            {"question":"Extras?","header":"Extras","multiSelect":true,
             "options":[{"label":"A","description":""},{"label":"B","description":""},{"label":"C","description":""}]}
        ]})
    }

    fn ans(selected: &[bool], other: Option<&str>) -> QuestionAnswer {
        QuestionAnswer {
            selected: selected.to_vec(),
            other_on: other.is_some(),
            other_text: other.unwrap_or_default().to_owned(),
        }
    }

    #[test]
    fn builds_answers_map_with_multiselect_joined() {
        let out = build_ask_input(
            &input(),
            &[ans(&[false, true], None), ans(&[true, false, true], None)],
        )
        .unwrap();
        assert_eq!(out["answers"]["Color?"], "Azul");
        assert_eq!(out["answers"]["Extras?"], "A, C");
        // Original questions are kept untouched.
        assert_eq!(out["questions"], input()["questions"]);
    }

    #[test]
    fn other_text_is_used_and_trimmed() {
        let out = build_ask_input(
            &input(),
            &[
                ans(&[false, false], Some("  verde ")),
                ans(&[false, true, false], Some("Z")),
            ],
        )
        .unwrap();
        assert_eq!(out["answers"]["Color?"], "verde");
        assert_eq!(out["answers"]["Extras?"], "B, Z");
    }

    #[test]
    fn incomplete_answers_give_none() {
        // Second question unanswered.
        assert!(
            build_ask_input(
                &input(),
                &[ans(&[true, false], None), ans(&[false; 3], None)]
            )
            .is_none()
        );
        // "Otra" selected but empty.
        assert!(
            build_ask_input(
                &input(),
                &[ans(&[false, false], Some("  ")), ans(&[true; 3], None)]
            )
            .is_none()
        );
        // Missing state / no questions.
        assert!(build_ask_input(&input(), &[]).is_none());
        assert!(build_ask_input(&json!({"questions":[]}), &[]).is_none());
        assert!(build_ask_input(&json!({}), &[]).is_none());
    }

    fn item(suggestions: Vec<Value>) -> PermissionItem {
        PermissionItem {
            request_id: "r1".into(),
            tool_name: "Bash".into(),
            description: None,
            input: json!({}),
            tool_use_id: None,
            suggestions,
            resolved: None,
        }
    }

    fn kinds(cs: &[Choice]) -> Vec<&ChoiceKind> {
        cs.iter().map(|c| &c.kind).collect()
    }

    #[test]
    fn tool_choices_with_add_rules() {
        let rule = json!({"type":"addRules","rules":[{"toolName":"Bash"}],"destination":"session"});
        let cs = tool_choices(&item(vec![rule.clone()]));
        assert_eq!(cs.len(), 3);
        assert_eq!(cs[0].label, "Yes");
        assert_eq!(cs[1].label, "Yes, and don't ask again");
        assert!(cs[1].tooltip.is_some());
        assert_eq!(*kinds(&cs)[1], ChoiceKind::Allow(vec![rule]));
        assert_eq!(cs[2].kind, ChoiceKind::Deny);
    }

    #[test]
    fn tool_choices_with_set_mode_only() {
        let m = json!({"type":"setMode","mode":"acceptEdits"});
        let cs = tool_choices(&item(vec![m.clone()]));
        assert_eq!(cs.len(), 3);
        assert_eq!(cs[1].label, "Yes, and accept edits this session");
        assert_eq!(cs[1].kind, ChoiceKind::Allow(vec![m]));
    }

    #[test]
    fn tool_choices_without_suggestions() {
        let cs = tool_choices(&item(vec![]));
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].kind, ChoiceKind::Allow(vec![]));
        assert_eq!(cs[1].kind, ChoiceKind::Deny);
    }

    #[test]
    fn plan_choices_lists_modes_then_deny() {
        let cs = plan_choices();
        assert_eq!(
            kinds(&cs),
            vec![
                &ChoiceKind::Plan(PermissionMode::Auto),
                &ChoiceKind::Plan(PermissionMode::AcceptEdits),
                &ChoiceKind::Plan(PermissionMode::Default),
                &ChoiceKind::Deny
            ]
        );
    }

    #[test]
    fn selection_clamps_and_digits_are_bounded() {
        assert_eq!(move_sel(0, -1, 3), 0);
        assert_eq!(move_sel(1, 1, 3), 2);
        assert_eq!(move_sel(2, 1, 3), 2);
        assert_eq!(move_sel(0, 1, 0), 0);
        assert_eq!(digit_index(1, 3), Some(0));
        assert_eq!(digit_index(3, 3), Some(2));
        assert_eq!(digit_index(4, 3), None);
        assert_eq!(digit_index(0, 3), None);
    }

    #[test]
    fn plan_input_replaces_only_plan_when_edited() {
        let inp = json!({"plan":"old","planFilePath":"p.md"});
        assert_eq!(plan_updated_input(&inp, None), inp);
        let out = plan_updated_input(&inp, Some("new"));
        assert_eq!(out["plan"], "new");
        assert_eq!(out["planFilePath"], "p.md");
    }
}
