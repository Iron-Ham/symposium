# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```sh
cargo build                    # Debug build
cargo build --release          # Release build
cargo test                     # Run all tests
cargo test <test_name>         # Run a single test by name
cargo test config::             # Run tests in a module
cargo clippy                   # Lint (must be clean — no warnings)
cargo check                    # Fast type-check without codegen
```

## What This Project Is

Symposium is a Rust implementation of the [OpenAI Symphony spec](https://github.com/openai/symphony/blob/main/SPEC.md). It's a long-running orchestration service that polls a Notion database for issues, creates isolated workspaces, and runs coding agent sessions against them. It diverges from the spec by using **Notion** (via an MCP server) instead of Linear.

## Architecture

The orchestrator runs a single-threaded `tokio::select!` event loop (`orchestrator/mod.rs`). State mutations are centralized through `OrchestratorState` (`domain/state.rs`), which wraps `Arc<Mutex<StateInner>>`. Config is broadcast via `tokio::sync::watch` for hot-reload.

**Data flow per tick** (`orchestrator/tick.rs`):
1. Check for stalled workers → mark failed
2. Drain ready retries
3. Spawn `NotionTracker` (MCP client) → fetch candidate issues
4. Filter by `dispatch::is_eligible` + sort by priority
5. For each eligible issue: create workspace → run hooks → build prompt (liquid) → spawn agent worker in `tokio::spawn`
6. Worker completion sends `OrchestratorEvent::WorkerCompleted` back via mpsc channel

**MCP proxy pattern**: All Notion communication goes through a spawned MCP server process (`tracker/mcp.rs`). `McpClient` speaks JSON-RPC 2.0 over line-delimited stdio. No `reqwest` — no direct HTTP to Notion.

**Agent protocol** (`agent/`): Spawns a coding agent as a subprocess, communicates via JSON-RPC stdio. 4-message handshake: `initialize` → `initialized` → `thread/start` → `turn/start`. Multi-turn loop in `worker::run_agent_attempt`. Client-side tool calls (e.g., `notion_query`) are intercepted and proxied to the tracker.

## Key Conventions

- **Error handling**: Use `crate::error::{Error, Result}` everywhere. Each module has its own `Error` variant (e.g., `Error::Tracker(String)`, `Error::Agent(String)`). Use `anyhow` only in `main.rs`.
- **TrackerClient trait** (`tracker/mod.rs`): Uses `#[allow(async_fn_in_trait)]` — not dyn-compatible. Use `NotionTracker` concretely when a trait object would be needed.
- **Config** (`config/schema.rs`): `ServiceConfig` is the central config struct. All sub-structs implement `Default` with sensible values. The `prompt_template` field holds the Liquid template body extracted from WORKFLOW.md.
- **Rust edition 2024**: Uses `let` chains in `if let` expressions (e.g., `if let Some(x) = ... && condition`). This is stable in Rust 1.85+.
- **Unsafe in tests only**: `std::env::set_var`/`remove_var` require `unsafe` blocks in edition 2024. This only appears in `config/env.rs` tests.
