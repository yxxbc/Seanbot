# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project state

Seanbot is a terminal AI agent (command name `sean`) written in Rust, talking to DeepSeek via an OpenAI-compatible API. The MVP described in `docs/superpowers/specs/2026-09-29-seanbot-mvp-design.md` is implemented as a three-crate workspace; the task-by-task plan is in `docs/superpowers/plans/2026-09-29-seanbot-mvp.md` (both are local-only: `docs/superpowers/` is git-ignored). Long-term concept notes (kernel, tools, knowledge base, subagents, plugins) are in `docs/Seanbot/`. All docs are written in Chinese.

## Commands

```bash
cargo build
cargo run -p seanbot-cli -- <args>        # e.g. `-- config`, `-- -p "问题"`, `-- models`
cargo test                                # all tests
cargo test -p seanbot-core <name>         # one crate, filtered by test name
cargo test -p seanbot-core --test agent   # agent-loop integration tests (fake provider)
cargo clippy --all-targets -- -D warnings
cargo fmt --all
```

CLI renderer tests use `insta` inline snapshots in `crates/seanbot-cli/src/render.rs`. Set `SEANBOT_HOME=<dir>` to point the CLI at a throwaway data dir instead of `~/.seanbot`.

## Architecture (MVP)

Cargo workspace with three crates and a strictly one-way dependency chain: `seanbot-cli → seanbot-core → seanbot-provider`. The binary lives in `crates/seanbot-cli/` (package `seanbot-cli`, `[[bin]] name = "sean"`).

- **seanbot-provider** — vendor layer. `Provider` trait (`info`, `list_models`, `stream`) returning `StreamChunk`s. Vendors are data-driven `ProviderDescriptor`s (`builtin_providers()`, `create()`); a new OpenAI-compatible vendor is just a new descriptor, vendor differences go in `Quirks`. Streaming tool-call fragments are assembled inside the provider; the core only ever sees complete `ToolCall`s with raw-string JSON arguments. Retries (3×, exponential backoff, honor `Retry-After`) happen only before the first chunk arrives. Knows nothing about agents or tool execution.
- **seanbot-core** — the agent loop, tool registry, the four builtin tools (`read`, `edit`, `bash`, `search`), bash denylist, config. Must never touch the terminal: its only UI surfaces are the `AgentEvent` stream (mpsc) and the `PermissionHandler` trait, so a future Tauri desktop app can reuse it. Entry point is `Agent::run_turn(input, events, cancel)`.
- **seanbot-cli** — input (rustyline) and rendering (crossterm spinners, ✓/✗ tool lines, usage line) only.

Invariants that span multiple components:

- **History consistency**: every assistant `tool_call` must end up with a matching tool message — including on cancel ("用户已取消"), errors, step limit (50 model calls/turn). Tool failures, denylist hits and invalid JSON args are returned to the model as tool results, never abort the turn.
- **Prefix-cache stability** (DeepSeek caches automatically): system prompt fixed for the whole session (no timestamps; `/clear` does not rebuild it), history is append-only, tool definitions serialized in identical order every request (`ToolRegistry` is a `BTreeMap`).
- **Builtin tools are immutable**: registering a tool whose name collides with a builtin is an error. `ToolSource` distinguishes `Builtin | AgentCreated | Plugin`.
- **`edit` requires a prior `read`** in the same session with unchanged file content (a length + hash fingerprint in `ReadTracker`; mtime is deliberately ignored so editor/formatter saves don't invalidate reads). Multi-match errors list occurrence line numbers and `occurrence` selects the Nth (mutually exclusive with `replace_all`); read-output line-number prefixes are stripped only as a fallback. Error text is deliberately terse (one line) — keep it that way.
- **bash denylist** is always enforced, before `PermissionHandler`, and can't be bypassed; tokenization failure means deny. Matching splits on `; && || | \n` and background `&`, extracts `$(...)`/backticks, strips env assignments and wrapper commands, basename-matches by word prefix, and blocks `curl|wget` piped into a shell. See spec §5.3 for exact rules.

Config and data live in `~/.seanbot/` (`config.toml` with mode `0600`, `history`); `DEEPSEEK_API_KEY` overrides the config key. Default model is `deepseek-flash`; DeepSeek requires `reasoning_content` to be echoed back on every request that carries tools (`Quirks::echo_reasoning`).

## Conventions

- User-facing UI text is Chinese; tool names and commands stay English.
- Library crates use `thiserror`; the CLI uses `anyhow`.
- Out of scope for the MVP (don't build unless asked): TUI/desktop, knowledge base, personas, subagents, plugins, context compaction, Markdown rendering. Session persistence (`-c`/`-r`/`/resume`, `seanbot-core::session`) and the tool confirmation UI are already implemented.

## Commit conventions (hard-enforced)

Format: `<type>(<scope>): <Chinese description>` — half-width colon, one space after it. Example: `feat(cli): 添加交互式对话`.
Allowed types: `feat fix docs style refactor perf test build ci chore revert`; scope is optional (`fix: 修正拼写`); `!` marks breaking changes (`feat(cli)!: ...`). The description must not end with `.` or `。`; ASCII-only first lines must be ≤ 72 chars.
One commit = one logical change; new files are committed individually.

Git hooks in `.githooks/` enforce this hard (install once per clone: `bash scripts/install-hooks.sh`, sets `core.hooksPath`):
- `commit-msg` — rejects messages that violate the format above (also full-width `：` and trailing punctuation).
- `pre-commit` — rejects files > 1 MiB, secret-like files (`.env`, `*.pem`, `*.key`, ...), and files containing merge-conflict markers.
Bypass (discouraged): `git commit --no-verify`.

`CHANGELOG.md` follows Keep a Changelog (`feat` → Added, `fix` → Fixed); user-visible changes update it in the same PR.
Issue / PR templates live in `.github/`; PRs must fill every checklist item.

