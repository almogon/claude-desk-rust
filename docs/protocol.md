# claude-deck protocol spike (Claude Code 2.1.326, Windows 11)

All facts below were captured from real runs. Raw captures are in `fixtures/` (`<case>.jsonl` = stdout, `<case>.stdin.jsonl` = what the driver sent). Driver scripts (Node): `driver.js`, `c1.js`, `c3.js`, `c4.js`, `c5.js`, `c7.js`, `c8.js`. Sandbox cwd: `spike\sandbox\`. One email address in `controls.jsonl` was redacted.

## 0. Executable resolution (Windows)

`Get-Command claude` -> `Application`, `C:\Users\user\.local\bin\claude.exe` (native .exe, version 2.1.326.0). Not an npm `.cmd` shim, so `Command::new("claude")` / `claude.exe` works without `cmd /c`. Node (`C:\Program Files\nodejs\node.exe`) and Python also exist. Spawning `claude.exe` from Node `child_process.spawn` with piped stdio worked; stderr was empty in all runs.

## 1. Flags used

Base: `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --model haiku`

Extra per case:
- `--permission-prompt-tool stdio --permission-mode default` (permission cases; the `stdio` value is accepted though not documented in `--help`)
- `--permission-mode bypassPermissions` (subagent)
- `--resume <session_id>` (resume)
- Noted in `--help` (not exercised except where stated): `--permission-prompts host|none` (default host), `--replay-user-messages`, `--fork-session`, `--forward-subagent-text`, `--include-hook-events`.

Important: the process, with no `--permission-mode`, started in `permissionMode: "plan"` here (inherited from the user's settings, NOT the CLI default). Always pass `--permission-mode` explicitly.

## 2. Client -> CLI messages (stdin, one JSON per line)

User message:
```json
{"type":"user","message":{"role":"user","content":"responde solo: hola"}}
```
`content` as plain string works. Slash commands (`/usage`) are sent the same way.

Control request (client -> CLI), request_id is client-chosen string:
```json
{"type":"control_request","request_id":"req_1","request":{"subtype":"set_model","model":"sonnet"}}
```
Control response to a CLI-initiated request (permissions):
```json
{"type":"control_response","response":{"subtype":"success","request_id":"<id from control_request>","response":{"behavior":"allow","updatedInput":{...original input...}}}}
{"type":"control_response","response":{"subtype":"success","request_id":"<id>","response":{"behavior":"deny","message":"text shown to the model"}}}
```
Both worked (see section 5). Closing stdin ends the process cleanly after the current turn.

## 3. CLI -> client messages (stdout)

All carry `session_id` (except a few system ones) and most a `uuid`.

### 3.1 `system/init` (emitted at the START OF EVERY TURN, not once per process)
Key fields: `session_id`, `model` (resolved id, e.g. `claude-haiku-5-5`), `permissionMode`, `cwd`, `original_cwd`, `tools` (array of names; contains `Task` while the tool_use name is `Agent`), `mcp_servers` [{name,status,source}], `slash_commands`, `skills`, `agents`, `plugins`, `claude_code_version`, `output_style`, `apiKeySource`, `capabilities`, `memory_paths`, `fast_mode_state`, `messaging_socket_path`, `powershell_path`. It is emitted only after the first user message arrives (not at process spawn).

### 3.2 `system/status`
`{"type":"system","subtype":"status","status":"requesting",...}` right before each API request. After a `set_permission_mode` it appears as `{"subtype":"status","status":null,"permissionMode":"acceptEdits"}`.
Other system subtypes seen: `thinking_tokens` (`estimated_tokens`, `estimated_tokens_delta`), `permission_denied` (`tool_name`,`tool_use_id`,`message`), `task_started`, `task_progress`, `task_updated`, `task_notification` (subagents, section 7).

### 3.3 `stream_event` (because of `--include-partial-messages`)
Wrapper of raw Anthropic SSE: `{"type":"stream_event","event":{...},"session_id","parent_tool_use_id":null,"uuid","api_message_id"}`.
`event.type` sequence per assistant message: `message_start` (carries `message.model` and initial usage), `content_block_start` (`content_block` type text/thinking/tool_use), `content_block_delta` (`delta.type` = `text_delta` {text} | `input_json_delta` {partial_json} | `thinking_delta` {thinking, estimated_tokens}; thinking text is empty / redacted-ish in this version), `content_block_stop`, `message_delta` (`delta.stop_reason`, usage), `message_stop`.
Order quirk: the full `assistant` message for a block arrives BEFORE the matching `content_block_stop`.

### 3.4 `assistant`
`{"type":"assistant","message":{"model","id","role":"assistant","content":[{"type":"text","text":"hola"}|{"type":"tool_use","id","name","input",...}|{"type":"thinking",...}],"usage":{...},"stop_reason":null},"parent_tool_use_id":null,"session_id","uuid","timestamp"}`
One `assistant` event is emitted per content block (a thinking block and a text block of the same API message come as separate events with the same `message.id`). `stop_reason` inside is null (real value only in `message_delta`).

### 3.5 `user` (tool results, emitted by the CLI)
`{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id","content":"...","is_error":false}]},"parent_tool_use_id":null,"tool_use_result":{...tool specific...},"tool_result_meta":[{"id","permission_decision":{"decision":"accept|reject","source":"config|user_temporary|user_reject",...}}]}`. Interrupt produces a `user` with text `[Request interrupted by user]`.

### 3.6 `rate_limit_event` - see section 9.

### 3.7 `result` (one per turn)
Fields: `subtype` (`success` | `error_during_execution`), `is_error`, `result` (final text), `session_id`, `stop_reason`, `terminal_reason` (`completed` | `aborted_streaming`), `num_turns`, `duration_ms`, `duration_api_ms`, `total_cost_usd` (cumulative for the process/session), `usage` {input_tokens, output_tokens, cache_creation_input_tokens, cache_read_input_tokens, ...}, `modelUsage` {<model>:{inputTokens,outputTokens,cacheReadInputTokens,cacheCreationInputTokens,costUSD,contextWindow,maxOutputTokens,...}}, `permission_denials` [{tool_name,tool_use_id,tool_input}], `subagent_stats`, `uuid`. For slash commands handled locally: `duration_api_ms:0`, `num_turns:0`, `total_cost_usd:0`.

## 4. Case `simple` and `multiturn`

`simple` sequence: `system/init`, `system/status(requesting)`, `stream_event` message_start, content_block_start, content_block_delta(text_delta "hola"), `assistant`, content_block_stop, message_delta, message_stop, `rate_limit_event`, `result/success`. Result text "hola".

`multiturn`: two user lines in the same process. The process stays alive; each turn yields its own `system/init` ... `result`. So exactly one `result` per user message, and a new `system/init` per turn. Turn 2 had no `rate_limit_event` (emitted only sometimes, see 9). Messages sent while a turn is running are queued (`interrupt` response has `still_queued`).

## 5. Case `permission`

- NO `initialize` control_request is required. First message after spawn works; `control_request` arrives mid-turn.
- Without `--permission-prompt-tool stdio` (fixture `permission_noflag`), a non-allowed Write is auto-denied: `system/permission_denied` event, no control_request. So the flag is required for interactive approval.
- Bash `echo hola` did NOT trigger a prompt: user's settings already allow it (`tool_result_meta.permission_decision.source:"config"`). Fixture `permission_echo_autoallowed`. Use Write (file creation in default mode) to trigger prompts; fixtures `permission_allow`, `permission_deny`.

CLI -> client request (exact):
```json
{"type":"control_request","request_id":"1f4ecb1b-...","request":{"subtype":"can_use_tool","tool_name":"Write","display_name":"Write","input":{"file_path":"...\\hola.txt","content":"hola"},"description":"hola.txt","permission_suggestions":[{"type":"setMode","mode":"acceptEdits","destination":"session"}],"server_policy_ask_only":true,"tool_use_id":"toolu_01PvVV..."}}
```
Sequence: `assistant`(tool_use) arrives, then `control_request` (it can come before or after `rate_limit_event`), CLI blocks until the response, then `user`(tool_result), `assistant`(text), `result`. Note: the assistant tool_use event can precede the control_request, so match by `tool_use_id`.

Allow (worked): `{"type":"control_response","response":{"subtype":"success","request_id":"<id>","response":{"behavior":"allow","updatedInput":<input>}}}` -> tool_result "File created successfully", `permission_decision.source:"user_temporary"`.
Deny (worked): `...,"response":{"behavior":"deny","message":"Denegado..."}` -> tool_result `{"content":"<message>","is_error":true}`, `non_execution_kind:"permission-rule"`, `permission_decision.source:"user_reject"`; the `result.permission_denials` lists the call; the model then continues and writes a final reply. Not tested here: `interrupt:true` in deny. `updatedPermissions` in allow: see section 11.

## 6. Case `controls` (client -> CLI control_requests)

| subtype | request | success response `response.response` |
|---|---|---|
| `initialize` | `{"subtype":"initialize"}` | big object: `commands`, `agents`, `models` [{value,resolvedModel,displayName,description,supportsEffort,...}], `account` {email,subscriptionType,organization}, `pid`, `current_permission_mode`, `session_state`, `claude_code_version`, `output_style`, ... Optional, works at any time. Useful for slash command list and model list. |
| `set_model` | `{"subtype":"set_model","model":"sonnet"}` | `{"model":"claude-sonnet-5-5"}` (alias resolved) |
| `set_permission_mode` | `{"subtype":"set_permission_mode","mode":"acceptEdits"}` | `{"mode":"acceptEdits"}` |
| `interrupt` | `{"subtype":"interrupt"}` | `{"still_queued":[]}` |

The client sends `initialize` at spawn (new and resumed sessions) to get `commands` for the slash-command autocomplete in the input box; if it fails, the name-only `slash_commands` from `system/init` are used.

Errors: `{"type":"control_response","response":{"subtype":"error","request_id","error":"...","error_code":"invalid_mode"|"unsupported_request"}}`. Valid modes: `acceptEdits, auto, bypassPermissions, default, dontAsk, plan`. Model aliases from `initialize`: default(opus), opus, sonnet, fable, haiku, plus full ids.

Effects: the next turn's `system/init` reports the new `model` and `permissionMode` (verified: `acceptEdits`, then `plan`; model `claude-sonnet-5-5` then back to haiku). The `status` event with `permissionMode` is emitted immediately after the change.

Interrupt mid-stream: response `success`, then a `user` event "[Request interrupted by user]", stream closes with message_stop, then `result` with `subtype:"error_during_execution"`, `is_error:true`, `terminal_reason:"aborted_streaming"`, `stop_reason:null`. The process stays alive and the next user message works normally (verified).

## 7. Case `subagent` (bypassPermissions)

The tool_use name is `Agent` (the init `tools` list calls it `Task`; handle both). Main assistant event:
`{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_01Hn...","name":"Agent","input":{"description":"Leer clave de nota.txt","subagent_type":"general-purpose","prompt":"...","run_in_background":false}}]},"parent_tool_use_id":null}`
Lifecycle:
1. `system/task_started`: `task_id` (= `agent_id`), `run_id`, `tool_use_id` (= the Agent tool_use id), `description`, `subagent_type`, `is_backgrounded`, `spawn_depth`, `task_type:"local_agent"`, `prompt`.
2. Subagent messages are `user` and `assistant` events (complete messages; NO stream_event partials were seen for the subagent) with `parent_tool_use_id` = the Agent tool_use id, plus `agent_id`, `subagent_type`, `task_description`. First one is the `user` event with the prompt.
3. `system/task_progress`: `task_id`, `tool_use_id`, `description` (e.g. "Reading nota.txt"), `last_tool_name`, `usage` {total_tokens, tool_uses, duration_ms}.
4. `system/task_updated`: `{"patch":{"status":"completed","end_time":<ms>}}`.
5. `system/task_notification`: `status:"completed"`, `summary` (final report text), `output_file`, `usage`.
6. Main-level `user` event with the Agent `tool_result` (`parent_tool_use_id` null, `tool_use_id` = the Agent id) holding the report wrapped in a "[Subagent hand-back]" frame; then the main model replies and `result` follows. `result.subagent_stats` summarises (`spawned`, `completed`, `by_type`).
The subagent also gets its own history file (section 8). Subagent models: the model/effort of the subagent can differ from the parent (here assistant events show `claude-sonnet-5-5` for the subagent while the parent was haiku).

## 8. History files (`~/.claude/projects`)

Layout: `C:\Users\user\.claude\projects\<cwd-with-separators-replaced-by->\<session_id>.jsonl`. Folder name = cwd with `:`, `\`, `/`, `.` and `_`-ish characters replaced by `-` (e.g. `C--work-project`, `C--Users-user-AppData-Local-Temp-...`); it is lossy, so read the real cwd from the file. Optional sibling directory `<session_id>\subagents\agent-<agentId>.jsonl` + `agent-<agentId>.meta.json` (`agentType`, `description`, `toolUseId`, `spawnDepth`, `finished`, `pickedEffortModel`); also `<session_id>\tool-results` in interactive sessions. Subagent lines have `isSidechain:true` and `agentId`.

Line types (each line has `type`): `user`, `assistant`, `attachment` (many sub-kinds: environment, model, instructions, skill_listing...; large, skip), `queue-operation` (enqueue/dequeue, `content` = prompt text, `timestamp`), `session-owner`, `last-prompt` (`lastPrompt`, `leafUuid`; rewritten after each turn - latest = most recent prompt), `cost-state` (`totalCostUSD`, `modelUsage`, durations), `ai-title` (`aiTitle`) and `agent-name` (`agentName`) - present in interactive sessions (observed title was just "claude" for a session), absent in headless ones; `mode`, `permission-mode`, `tools-used`, `file-history-snapshot`, `api-request*` (large, skip), `system`, `atis-latch`. In practice no `summary` type was seen.

Useful fields on `user`/`assistant` lines: `sessionId`, `uuid`, `parentUuid`, `timestamp` (ISO UTC), `cwd`, `gitBranch`, `version`, `entrypoint` (`sdk-cli`, `cli`...), `permissionMode`, `isSidechain`, `isMeta` (skip meta user lines when looking for the first real prompt), `message` (Anthropic format; user content is a string or block array; tool_result user lines have `sourceToolAssistantUUID`).
Recipe for a session list: cwd = first line having `cwd`; first prompt = first `user` line with `isMeta` not true and `isSidechain` false and string/text content not starting with `<` (or the first `queue-operation` enqueue `content`); timestamp = first line `timestamp` (start) and file mtime (last activity); title = last `ai-title`, fallback to `last-prompt` / first prompt; session id = file stem (also `sessionId` field). Files can be large (>1 MB with attachments/api-request); read only the head and tail.
Sanitized sample: `fixtures/history_sample.jsonl`.

## 9. Usage / rate limits

`rate_limit_event`:
```json
{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1794940800,"rateLimitType":"five_hour","overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false,"unifiedWindows":{"five_hour":{"utilization":0.17,"resetsAt":1794940800,"status":"allowed"},"seven_day":{"utilization":0.09,"resetsAt":1795446000,"status":"allowed"}}},"uuid","session_id"}
```
`utilization` is a fraction 0..1 (0.17 = 17%, matching `/usage` "Current session: 17% used"). `resetsAt` is Unix epoch seconds. `unifiedWindows` has `five_hour` and `seven_day` (a later event may include only changed windows; in the first captures five_hour was present everywhere, seven_day mostly). Emitted irregularly: once per turn in most cases, but not on every turn and not on locally handled slash commands. No other rate-limit/utilization fields exist in result/usage events (grep over all fixtures: only `rate_limit_event`).

Slash commands in headless: `/usage` WORKS and returns a text report (synthetic `assistant` with `model:"<synthetic>"`, `local_command_outcome`, then `result` with the text in `result`, cost 0): "Current session: 17% used · resets Nov 17, 7:39pm (UTC) / Current week (all models): 10% used · resets Nov 23, 3:59pm / what's contributing ..." Human text, parseable but locale/format dependent. `/cost` is an alias of `/usage` (same output). `/status` returns "/status isn't available in this environment." (`local_command_outcome.kind:"unavailable_headless"`).
So for a usage meter prefer `rate_limit_event.unifiedWindows` (structured); use `/usage` as optional detailed text.

## 10. Case `resume`
`--resume efbc96b7-...` + a new user message: works, the model remembered the first turn; `system/init.session_id` and `result.session_id` are the SAME original id (no new id unless `--fork-session`, not tested). The history file of that session gets the new turn appended.

## Surprises / open questions

1. `--model haiku` is not reliably honored per turn: `system/init.model` said `claude-haiku-5-5` but some turns' `assistant.message.model` / `modelUsage` were `claude-sonnet-5-5` (simple, multiturn turn 1, controls) while others (permission, subagent main agent) were true haiku. The history `api-request` showed `requestedModel: claude-sonnet-5-5` with `advisorModel: claude-opus-5-5`. Cause unknown (possibly a user setting/advisor feature); UI should display the model from `assistant.message.model`/`modelUsage`, not `init.model`.
2. Default permission mode is inherited from user settings (`plan` here), not "default". Always pass `--permission-mode`.
3. `system/init` repeats every turn; and `tools` lists `Task` while the call is named `Agent`.
4. Subagent internals arrive only as whole messages (no partial stream events) unless `--forward-subagent-text` changes this (not tested).
5. Thinking text is empty in streams (only `estimated_tokens`); the signature is present.
6. Not tested: deny with `interrupt`, image content in user messages, `--replay-user-messages`, `--fork-session`, `--permission-prompts none`, concurrent control_requests, behaviour when the client does not answer a control_request, Ctrl-C/kill semantics, running with a spaces-containing cwd.
7. `rate_limit_event` frequency/trigger is unclear (not every turn). `resetsAt` for five_hour matched `/usage` (time zone handling not verified exactly).
8. `total_cost_usd` is cumulative per session across resumes? (resume fixture shows value continuing from history `cost-state`; not verified).
9. Fixtures contain local paths and full CLAUDE.md-derived slash command lists; share with care.

## 11. Case `permission_always` ("Permitir siempre": allow + `updatedPermissions`)

Fixture: `tests/fixtures/permission_always.jsonl` (+ `.stdin.jsonl`). Same flags as section 5, cwd = empty folder, Bash `mkdir d1` (not auto-allowed, unlike `echo`) in turn 1, `mkdir d1 d3` in turn 2 of the same process.

`permission_suggestions` for a Bash call (real capture, 3 entries):
```json
[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"mkdir d1 *"}],"behavior":"allow","destination":"localSettings"},
 {"type":"addDirectories","directories":["<cwd>"],"destination":"session"},
 {"type":"setMode","mode":"acceptEdits","destination":"session"}]
```
For Write the only suggestion is `[{"type":"setMode","mode":"acceptEdits","destination":"session"}]` (section 5).

Allow with the rule applied (worked, CLI accepts it):
```json
{"type":"control_response","response":{"subtype":"success","request_id":"<id>","response":{"behavior":"allow","updatedInput":<input>,"updatedPermissions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"mkdir d1 *"}],"behavior":"allow","destination":"localSettings"}]}}}
```
`updatedPermissions` is simply (a subset of) `permission_suggestions` echoed back verbatim.

Findings:
- Echoing only the `addRules` entry: the second Bash call in the same process (`mkdir d1 d3`, matches the rule) triggered NO `can_use_tool`.
- `destination:"localSettings"` persists the rule in `<cwd>/.claude/settings.local.json` (`{"permissions":{"allow":["Bash(mkdir d1 *)"]}}`), i.e. project-local, NOT the user's global settings; it also survives across processes (a new process in the same cwd did not ask again).
- Same entry with `destination:"session"` (rewritten by the client): accepted, no file written, second call not asked; lasts only for that process.
- Echoing the Write `setMode acceptEdits` (session) suggestion: accepted; the second Write (other file) was not asked. Note it changes the whole session mode to acceptEdits (all edits auto-allowed), not a per-tool rule.
- Echoing all 3 Bash suggestions also worked, but `setMode` makes the 2nd call auto-allowed for a broader reason than the rule.
- The CLI does not send a `control_response` ack or extra event for `updatedPermissions`; the only visible effect is the later absence of `can_use_tool`.
- Recommendation for the UI: "Permitir siempre" = allow + `updatedPermissions` containing the first suggestion of type `addRules` if present (keep its `destination`, `localSettings` is project-local); if the request has no `addRules` suggestion, send the suggestions as-is only if the user accepts the mode change, otherwise fall back to plain allow.

## 12. Case `image` (user message with an image block)

Real run (Claude Code 2.1.326, `--model haiku --permission-mode default`, empty cwd, `-p --input-format stream-json --output-format stream-json --verbose`): a `user` message whose `content` is an array with an `image` block followed by a `text` block is ACCEPTED, and the model sees the image (a 200x120 red PNG with the white word "HOLA" was described correctly: `La imagen muestra la palabra "HOLA" en letras blancas sobre un fondo rojo.`). No fallback to temp files / `@path` is needed.

Format sent by claude-deck (one line on stdin; the base64 payload is omitted here):
```json
{"type":"user","message":{"role":"user","content":[
  {"type":"image","source":{"type":"base64","media_type":"image/png","data":"<base64>"}},
  {"type":"text","text":"<pasted_text>\n...long pasted text...\n</pasted_text>"},
  {"type":"text","text":"user's own text"}]}}
```
Order: images, pasted texts (wrapped in `<pasted_text>`), user text last. PNGs over 5 MB are downscaled to 2000 px on the longest side before encoding. Serialization is covered by the unit test `user_message_with_attachments_serializes_blocks` (no fixture is stored because of the payload size). In history files image blocks appear as `{"type":"image",...}` inside user messages and pasted texts as `<pasted_text>` text blocks; the history view renders both as chips.

## 13. AskUserQuestion and ExitPlanMode

Fixtures: `tests/fixtures/ask_answer.jsonl` and `plan_edit.jsonl` (+ `.stdin.jsonl`). Real runs (Claude Code 2.1.326 / 2.1.328, `--model haiku`, `--permission-prompt-tool stdio`, empty cwd). Both tools arrive as a normal `can_use_tool` control_request with `requires_user_interaction:true`. In `plan_edit.jsonl` the partial `stream_event` lines were dropped (their fragments split paths).

### 13.1 AskUserQuestion (mode `default`)
Request (abbreviated):
```json
{"type":"control_request","request_id":"<id>","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{"questions":[{"question":"¿Qué color prefieres: rojo o azul?","header":"Color","options":[{"label":"Rojo","description":"..."},{"label":"Azul","description":"..."}],"multiSelect":false}]},"tool_use_id":"toolu_...","requires_user_interaction":true}}
```
Answer (worked): allow with `updatedInput` = the original input plus an `answers` object keyed by the full question text, value = the chosen label (or free text):
```json
{"type":"control_response","response":{"subtype":"success","request_id":"<id>","response":{"behavior":"allow","updatedInput":{"questions":[...unchanged...],"answers":{"¿Qué color prefieres: rojo o azul?":"azul"}}}}}
```
The `tool_result` the model gets: `The user answered: "<question>"="azul". Read the answers carefully ...`, and the final reply was "Azul.". Multi-select answers and the "Other" free-text case were NOT live-tested (the client joins several labels with ", ").

### 13.2 ExitPlanMode (mode `plan`)
The request input has exactly two fields: `plan` (markdown text) and `planFilePath` (the CLI has already saved the plan to `~/.claude/plans/<name>.md` with a Write call before):
```json
{"subtype":"can_use_tool","tool_name":"ExitPlanMode","input":{"plan":"# Plan: ...","planFilePath":"C:\\Users\\user\\.claude\\plans\\<name>.md"},"tool_use_id":"toolu_...","requires_user_interaction":true}
```
Approve: allow with `updatedInput` = input with `plan` replaced by the edited text. The edited plan IS honored: the model got `User has approved your plan ... ## Approved Plan (edited by user): <edited text>` (`tool_use_result.planWasEdited:true`) and created the extra file added by the edit. The plan file on disk is not rewritten; only the text sent to the model changes.

Mode after approval: the client sent `{"subtype":"set_permission_mode","mode":"acceptEdits"}` right after the allow response (before the tool_result arrived); the CLI answered `{"mode":"acceptEdits"}`, emitted `system/status` with `permissionMode:"acceptEdits"`, and the following Write calls ran without a prompt (`permission_decision.reason_type:"mode"`). Which mode the CLI picks on its own after an approval when the client does not send `set_permission_mode` was not tested.
