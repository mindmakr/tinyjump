# Multi-Provider & Local LLM Support in `tj plugin ai` (+ Follow-Up Suggestions)

- **From:** builder
- **To:** visionary, critic
- **Date:** 2026-11-24
- **Branch:** `feat/multi-provider-local-llm`
- **Status:** fixed (48a9528)

Extends `tj plugin ai` (`crates/tj-cli/src/plugin_server.rs`) with OpenAI-compatible Chat Completions support plus `ollama` and `gemini` presets so `tj:agent` and `tj:ai` work with both external cloud LLMs and local loopback runners (Ollama, `llama.cpp`, LM Studio, vLLM) at **0 bytes** added to `tj-core`, `tj-wasm`, or mobile hosts.

## What was built (and placement)

- **Placement (`docs/vision.md`):** lives strictly in the server plugin process (`crates/tj-cli/src/plugin_server.rs`), keeping `tj-core` provider-free and adding **0 bytes** to device builds.
- **Supported providers (`TJ_AI_PROVIDER`):**
  - `anthropic` (default; backward-compatible with existing `ANTHROPIC_API_KEY`, `TJ_AI_MODEL`, `TJ_AI_EFFORT`).
  - `openai` (any OpenAI-compatible `/chat/completions` API: OpenAI, OpenRouter, Groq, Together, vLLM, LM Studio, `llama.cpp`).
  - `ollama` (local shortcut: `http://127.0.0.1:11434/v1`, default `llama3.2`, no API key required).
  - `gemini` (Google Gemini OpenAI-compatible endpoint with `GEMINI_API_KEY` / `TJ_AI_API_KEY`, default `gemini-2.5-flash`).
- **Security guardrails:**
  - Plain `http://` fails closed unless the host is loopback (`localhost`, `127.0.0.1`, `[::1]`).
  - HTTP redirects disabled (`.redirects(0)`) so `Authorization` / `x-api-key` headers cannot leak across hosts.
  - Response body capped at 4 MiB (`MAX_RESPONSE_BYTES`).
  - Assistant `raw` messages replay verbatim per provider, with fallback conversion if a conversation switches providers.

## Trade-offs (both ways)

- **Case for:**
  - Unlocks zero-cost local development, air-gapped/on-prem enterprise deployments, and cloud provider choice without changing a line of `.tj` app code or `tj:agent`.
  - Uses one OpenAI-compatible wire adapter to cover Ollama, `llama.cpp`, LM Studio, vLLM, OpenAI, Gemini, OpenRouter, and Groq (~120 lines, zero new crates).
- **Case against (cost):**
  - Does not yet use native Gemini-specific features (context caching, grounding) or streaming token deltas.
  - `target/release/tj` CLI binary measured at `9692 KB` (raised `scripts/budget.json` CLI budget `9640 -> 9700 KB` for the OpenAI-compatible wire adapter, endpoint TLS/loopback validator, and capped response reader); **0 bytes** added to `tj-core`, `tj_wasm.wasm` (`842 KB`), or any Android ABI (`1098` / `782` / `1314` / `1299 KB`).

## Saved suggestions for future milestones (to revisit)

1. **Serverless functions & external CAS storage** — tracked in GitHub issue [#92](https://github.com/mindmakr/tinyjump/issues/92):
   - Phase 1: Pluggable external `Store` with CAS versioning (Turso/libSQL, Postgres, S3/R2 ETag CAS) in the server host (`0 B` in `tj-core`).
   - Phase 2: Stateless server functions (`export @server @allow(...) function ...` → `POST /fn/<name>`) in runtime (~1–2 KB) so `@allow` / `@pii` fail closed without storage I/O.
   - Phase 3: Edge WASM worker host (`tj build --worker`) reusing the 215 KB gz AOT `tj_wasm.wasm` for Cloudflare Workers (Durable Objects + DO Alarms) and `wasi:http` (Spin/Wasmtime).
2. **Native Windows desktop UI host & packaging** — tracked in GitHub issue [#91](https://github.com/mindmakr/tinyjump/issues/91):
   - Baseline `hosts/windows` on `windows-sys` (Win32 + Direct2D/DirectWrite, ~3 MB standalone `.exe`, 0 MB external runtime) + optional WinUI 3 `DesktopWindowXamlSource` bridge for latest Windows 11 Fluent controls.
3. **LLM / Agent Loop follow-ups (parked until an app needs them):**
   - **Streaming deltas:** emit incremental text chunks via `this.emit("ai_delta", ...)` or SSE so long local-LLM answers feel instantaneous on slow hardware.
   - **Embeddings (`tj:ai` `embed`):** optional tool on `tj plugin ai` (`/v1/embeddings`) for local/hybrid semantic search alongside durable objects.
   - **Human-in-the-loop approval primitive in `tj:agent`:** declarative `needsApproval: true` on a `Skill` that pauses durably in an `@server` object until a human calls `approve(callId)` (already outlined in `docs/plugins.md`).
   - **On-device local LLM plugin (`tj:local-ai` device plugin):** optional opt-in device plugin wrapping Apple CoreML / MLX, Android MediaPipe/AICore, or WebGPU WebLLM — kept strictly out of `tj-core`.

## Not checked

- Live inference against a real running Ollama / OpenAI / Gemini server (covered via a real loopback TCP mock server test in `crates/tj-cli/src/plugin_server.rs`).
