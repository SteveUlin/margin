# observability for llm / agent sessions: landscape, 2026-11-08

warrant key: **[read: url]** = i fetched that page or file this session; **[ran]** = `gh api` / `curl` output this session (stars, licence, last push as of 2026-11-09 02:49 UTC); **[inferred]** = my reasoning or prior knowledge, not checked this session.

## headline

- every major platform now ships a **claude code integration**, and they split into two routes: (1) a hook plugin that parses the transcript JSONL (langfuse, opik, phoenix), or (2) claude code's native OTLP export, whose **traces are still beta** and need `CLAUDE_CODE_ENHANCED_TELEMETRY_BETA=1`. [read: https://www.comet.com/docs/opik/integrations/claude-code, https://langfuse.com/integrations/developer-tools/claude-code, https://arize.com/docs/phoenix/integrations/coding-agents/claude-code]
- the hook-plugin route wins on content: native OTel redacts prompts, tool input and output by default, and opik reports assistant output on the OTel path works only in `claude -p` / the agent sdk, not interactive sessions. [read: opik page above]
- nobody in the platform tier does **live, local, terminal-side** display. they all ship traces off to a web ui after the turn. the local-first niche belongs to small tools: ccusage, Claude-Code-Usage-Monitor, agentsview, disler's hook dashboard. that gap is margin's niche.

## claude code's own telemetry (the source everything else reads)

[read: https://code.claude.com/docs/en/monitoring-usage]

- **enable**: `CLAUDE_CODE_ENABLE_TELEMETRY=1`, then `OTEL_METRICS_EXPORTER` (otlp/prometheus/console), `OTEL_LOGS_EXPORTER` (otlp/console), `OTEL_EXPORTER_OTLP_PROTOCOL` (grpc, http/json, http/protobuf; no default), `OTEL_EXPORTER_OTLP_ENDPOINT`. logs export every 5 s, metrics every 60 s by default.
- **metrics**: `claude_code.session.count`, `.cost.usage` (USD), `.token.usage` (input/output/cacheRead/cacheCreation), `.lines_of_code.count`, `.commit.count`, `.pull_request.count`, `.code_edit_tool.decision`, `.active_time.total`.
- **events** (via the logs exporter): `user_prompt`, `assistant_response`, `tool_result` (tool_name, tool_use_id, success, duration_ms, error_type, input/result size bytes), `api_request` (model, cost_usd, duration_ms, the four token counts, request_id), `api_error`, `api_refusal`, `tool_decision` (accept/reject plus source: config, hook, user_permanent, user_temporary, user_abort, user_reject), `safety_event`, `subagent_completed`, `mcp_server_connection`, `plugin_loaded`, and raw `api_request_body` / `api_response_body` behind `OTEL_LOG_RAW_API_BODIES` (which also accepts `file:<dir>`).
- **traces (beta)**: `CLAUDE_CODE_ENHANCED_TELEMETRY_BETA=1` + `OTEL_TRACES_EXPORTER=otlp`. span tree: `claude_code.interaction` (one per prompt) > `claude_code.llm_request`, `claude_code.hook`, `claude_code.tool` > `tool.blocked_on_user`, `tool.execution`, subagent spans. it propagates W3C `TRACEPARENT` into Bash subprocesses.
- **content gates**: `OTEL_LOG_USER_PROMPTS`, `OTEL_LOG_ASSISTANT_RESPONSES`, `OTEL_LOG_TOOL_DETAILS`, `OTEL_LOG_TOOL_CONTENT`.
- **correlation keys**: `session.id` (`/clear` replaces it), `prompt.id` (a UUID linking every event of one prompt), `event.sequence`, `tool_use_id` (joins `tool_result`, `tool_decision` **and hooks**), `message.uuid` (matches transcript entries).
- **hooks reference**: https://code.claude.com/docs/en/hooks [inferred: url pattern; not fetched]. **transcripts**: JSONL under `~/.claude/projects/` [read: agentsview readme; the langfuse hook gets `transcript_path` on stdin, read: langfuse page].
- official example stack: `anthropics/claude-code-monitoring-guide`, 388 stars, last push 2025-07 [ran].

## table

| name | what | ingest | self-host | licence | stars | link |
|---|---|---|---|---|---|---|
| **Langfuse** | traces, sessions, generations w/ tokens and cost, evals, prompt mgmt, datasets | SDKs (py/ts), OTLP endpoint; **claude code: Stop-hook plugin that tails the transcript** | yes (docker/k8s) | MIT core + `ee/` dirs commercial; copyright now "ClickHouse, Inc." [ran: LICENSE] | 37,168 [ran] | https://github.com/langfuse/langfuse · plugin https://github.com/langfuse/claude-observability-plugin (30★) |
| **Opik** (Comet) | traces/spans, threads (sessions), cost, evals, LLM-judge metrics, prompt opt | SDKs, OTLP; **claude code: hook plugin + native-OTel route** | yes, full platform | Apache-2.0 [read: README] | 23,268 [ran] | https://github.com/comet-ml/opik · plugin https://github.com/comet-ml/opik-claude-code-plugin (25★) |
| **Arize Phoenix** | traces, evals, datasets, experiments; runs locally on :6006 | OTel + OpenInference instrumentation; **claude code: `coding-harness-tracing` hook plugin (16 hooks)** | yes, also runs as a local process | Elastic License 2.0 (source-available) [ran: LICENSE] | 12,206 [ran] | https://github.com/Arize-ai/phoenix · plugin https://github.com/Arize-ai/coding-harness-tracing (62★) |
| **OpenInference** | OTel instrumentations + span-kind conventions (LLM, TOOL, AGENT, CHAIN…) used by Phoenix | OTel | n/a (library) | Apache-2.0 | 1,309 [ran] | https://github.com/Arize-ai/openinference |
| **OpenLLMetry** (Traceloop) | OTel auto-instrumentation for LLM providers and vector DBs; seeded the OTel GenAI semconv | OTel SDK, exports to any OTLP backend | n/a (library); Traceloop cloud separate | Apache-2.0 [read: README] | 7,527 [ran] | https://github.com/traceloop/openllmetry |
| **OTel GenAI semconv** | standard span/metric/event names for GenAI clients, agents, MCP | spec | n/a | Apache-2.0 | 527 (new dedicated repo) [ran]; old repo 684 | https://github.com/open-telemetry/semantic-conventions-genai |
| **Helicone** | AI gateway + request logging, cost, caching, prompt mgmt | **proxy** (change baseURL) or async logging | yes (docker) [read: README] | Apache-2.0 [ran] | 6,273 [ran] | https://github.com/Helicone/helicone |
| **AgentOps** | agent sessions, "session replay", step graphs, cost | python SDK (decorators, framework hooks) | yes; app is MIT [read: README] | MIT | 5,949; last push 2026-06 (slowing) [ran] | https://github.com/AgentOps-AI/agentops |
| **MLflow** (tracing) | traces, evals, experiment tracking | SDK autolog, OTel | yes | Apache-2.0 | 28,726 [ran] | https://github.com/mlflow/mlflow |
| **Laminar** | OTel-native tracing, evals, browser-agent session recordings | OTel SDK | yes (docker compose) [read: README] | Apache-2.0 | 3,478 [ran] | https://github.com/lmnr-ai/lmnr |
| **Agenta** | prompt playground, evals, tracing | SDK, OTel | yes | MIT core + `ee/` [ran: LICENSE] | 4,894 [ran] | https://github.com/Agenta-AI/agenta |
| **OpenLIT** | OTel-native LLM observability + GPU metrics | OTel SDK | yes | Apache-2.0 | 2,970 [ran] | https://github.com/openlit/openlit |
| **Logfire** (Pydantic) | OTel wrapper w/ LLM views; has a **Rust SDK** | OTel | platform closed; self-host = enterprise licence [read: README] | MIT (SDKs) | 4,539 [ran] | https://github.com/pydantic/logfire |
| **LangSmith** | traces, evals, datasets, annotation queues | SDK, OTel | enterprise only [read: Opik README comparison table, vendor-authored] | SDK MIT; platform closed | 1,083 (SDK) [ran] | https://github.com/langchain-ai/langsmith-sdk |
| **W&B Weave** | traces, evals, scorers | SDK (`@weave.op`) | enterprise only [read: Opik README table, vendor-authored] | Apache-2.0 (SDK) | 1,145 [ran] | https://github.com/wandb/weave |
| — **claude-code-native, local-first** — | | | | | | |
| **ccusage** | token/cost reports per day/session/5h block from transcripts | reads `~/.claude/projects` JSONL [inferred: known behaviour] | local CLI | licence file in `apps/ccusage/` [ran] | 19,368 [ran] | https://github.com/ccusage/ccusage |
| **Claude-Code-Usage-Monitor** | live terminal burn-rate, predicts limit hit | reads transcripts [inferred] | local TUI (python) | MIT | 8,797 [ran] | https://github.com/Maciek-roboblog/Claude-Code-Usage-Monitor |
| **agentsview** | session search (SQLite FTS5), token/cost stats, heatmaps, live SSE updates, 8 MCP tools, 50+ agents | watches `~/.claude/projects/` | local web ui :8080 + Tauri desktop; Go + Svelte | MIT [read: README] | 6,406 [ran] | https://github.com/kenn-io/agentsview |
| **claude-code-hooks-multi-agent-observability** (disler) | live event timeline across sessions, pulse chart, transcript viewer | 12 hook events → `send_event.py` → HTTP POST to Bun server → SQLite → WebSocket → Vue | local | none declared [ran] | 1,566 [ran] | https://github.com/disler/claude-code-hooks-multi-agent-observability |
| **claude-code-transcripts** (simonw) | renders transcripts to HTML | JSONL | local CLI | Apache-2.0 | 1,729; last push 2026-02 [ran] | https://github.com/simonw/claude-code-transcripts |
| **claude-code-log** | transcript JSONL → HTML/TUI log | JSONL | local | MIT | 1,256 [ran] | https://github.com/daaain/claude-code-log |
| **claude-code-otel** | docker stack (collector + prometheus + grafana) for CC metrics | native OTel | local | MIT | 521; stale since 2025-06 [ran] | https://github.com/ColeMurray/claude-code-otel |
| **claude-squad** | multi-agent terminal manager (tmux + worktrees), adjacent not observability | n/a | local | AGPL-3.0 | 8,753 [ran] | https://github.com/smtg-ai/claude-squad |

stars for `semantic-conventions-genai` differ by fetch (527 via api); gh search hit the unauthenticated rate limit, so i didn't run a broad repo sweep. treat the local-first rows as a known-names list, not an exhaustive survey.

## what the platforms converge on [inferred from the rows above]

- **unit = turn**. langfuse, opik and phoenix all make one trace per user prompt, nest generations and tool spans under it, and group turns by `session_id`. claude code's native span tree uses the same shape (`claude_code.interaction`).
- **joins by `tool_use_id`**. the langfuse hook matches tool results to calls by it; claude code puts it on OTel events and hooks alike.
- **incremental transcript tailing**. the langfuse hook keeps a byte offset per transcript, buffers partial lines, restarts if the file shrinks, dedupes assistant messages by message id (keeping the latest), caps text at 20,000 chars, and fails open with a 5 s flush cap. [read: langfuse page]
- **redaction gates per content class** (prompts / responses / tool details / tool content), off by default on claude code's side, on by default on phoenix's side.

## ideas for margin

margin today registers `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop` [read: `hooks/settings.example.json`] and doesn't appear to touch tokens, cost or the transcript (grep of `hook.rs` for transcript/tool_use_id/cost/token found nothing) [ran].

1. **turn as the top-level row of the activity feed.** every platform converged on "one prompt = one trace, tools nested." margin's live tool activity could group under the current turn (opened by `UserPromptSubmit`, closed by `Stop`) and collapse finished turns into one line: `turn 7 · 12 tools · 2m10s · $0.41`. borrow the shape, not the web ui.
2. **cost and tokens per turn from the transcript, not OTel.** at `Stop`, tail `transcript_path` from the hook payload using the langfuse pattern (byte offset per session, partial-line buffer, reset on shrink, dedupe by message id) and sum `usage` on assistant messages. this works in interactive sessions with zero env setup, unlike the OTel route. [inferred: assumes assistant entries carry `usage`, which the langfuse hook relies on]
3. **an optional local OTLP receiver feed.** a `margin otel` feed listening on localhost:4318 (http/json) gets `api_request` cost, `tool_result.duration_ms`, `tool_decision` source and `api_error`/`api_refusal` for free, keyed by `prompt.id` and `tool_use_id`. ship it as an opt-in feed because it requires env vars in claude code's launch environment, not settings.json from a repo.
4. **`blocked_on_user` as a first-class state.** claude code's span tree separates `tool.blocked_on_user` from `tool.execution`. margin should show "waiting on you" distinctly from "running" (via `PermissionRequest`/`Notification` hooks, which margin doesn't register yet) because that's the one state sulin must act on.
5. **subagents as nested lanes.** opik nests subagents under the parent `Task` span; phoenix nests them under the turn; disler filters by agent "swim lane." register `SubagentStart`/`SubagentStop` and indent their tool activity under the Agent tool call that spawned them.
6. **failure and rejection markers.** add `PostToolUseFailure`, and surface `tool_decision` sources (user_reject vs hook vs config). a red mark on rejected/failed tools gives the scrollback the "where did it go wrong" affordance that trace uis spend a whole page on.
7. **emit, don't just display: write margin events as OTel-shaped JSONL.** if margin's event log used GenAI-semconv-ish names (`gen_ai.operation.name=execute_tool`, `gen_ai.tool.call.id`), any of langfuse/phoenix/opik could ingest margin's log later without margin owning a backend. cheap insurance; keep it a file, not a network exporter. [inferred: semconv attribute names from prior knowledge; the fetched pages didn't list them]
8. **fail open with a hard time cap in every hook.** langfuse's hook logs errors and never blocks claude code, with a 5 s flush cap. margin's hooks should hold the same contract explicitly (and test it): a dead daemon must cost a hook nothing.
9. **don't build search/analytics.** agentsview (6.4k★, MIT, SQLite FTS5 over `~/.claude/projects`) and ccusage (19k★) already own retrospective search and cost reports. margin's value is the *live, beside-the-chat, interactive* layer none of them have; link out (`margin` could open agentsview at the session) rather than duplicate.
10. **per-session colour identity.** disler's dashboard puts an app colour and a session colour on two borders so concurrent sessions stay distinguishable. useful when margin watches several agents (the agent-monitor feed).

## sources

- https://code.claude.com/docs/en/monitoring-usage
- https://langfuse.com/integrations/developer-tools/claude-code
- https://www.comet.com/docs/opik/integrations/claude-code
- https://arize.com/docs/phoenix/integrations/coding-agents/claude-code
- https://github.com/open-telemetry/semantic-conventions-genai
- https://github.com/disler/claude-code-hooks-multi-agent-observability
- https://github.com/kenn-io/agentsview
- READMEs/LICENSEs fetched raw from github for helicone, agentops, weave, openllmetry, opik, langsmith-sdk, lmnr, logfire, langfuse, phoenix, agenta
