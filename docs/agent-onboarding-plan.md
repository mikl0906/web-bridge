# Plan: agent onboarding — from "hub installed" to "agents just use it"

## Problem

The hub, the SDK, and an example app exist, but nothing teaches agents (Claude
Code, other MCP clients, shell-capable agents) that the bridge exists or how to
use it. Today a user must know to run `claude mcp add` per app instance, with a
URL that only works while that app is connected.

## Target experience

1. User installs and opens the hub.
2. The hub window shows a **"Connect your agent"** panel: one button (or one
   copy-paste command) registers the bridge with Claude Code — once per
   machine, covering every current and future app.
3. From then on everything is runtime discovery: the agent lists connected
   apps, reads their self-served descriptions/instructions/schemas, and calls
   tools. The user never explains an app to an agent.

Guiding principle (same as the rest of the design): **per-app knowledge lives
in the app** and travels through the protocol. The client side only needs to
learn the protocol once. Consequently: one general skill, no per-app skills —
if an app is confusing to agents, the fix is that app's `instructions` field.

## Workstreams

### 1. Hub-level meta-MCP endpoint (`POST /mcp`) — the linchpin

A single MCP server for the whole hub, always up (even with zero apps
connected), so one registration is a stable, long-lived contract.

- Route: `POST /mcp` in `web-bridge/src-tauri/src/server.rs`, following the
  existing minimal MCP slice (`initialize`, `ping`, `tools/list`,
  `tools/call`; JSON responses, no SSE). `mcp` is already in `RESERVED_SLUGS`,
  so the route can't collide with an app name.
- Meta-tools (names TBD, keep them few):
  - `list_apps` — the discovery document: connected apps, descriptions,
    instructions, instances, tool names. When nothing is connected, return a
    friendly explanation ("no apps connected — apps appear here when the user
    opens them in a browser"), not an error.
  - `get_tools` `{ app, instanceId? }` — full tool catalog with input schemas
    (same data as `GET /{app}/tools`).
  - `call` `{ app, instanceId?, tool, payload }` — dispatch through the same
    `AppState::call` path as REST and per-instance MCP. Reuse the existing
    `HubError` → tool-error mapping (`isError: true` with the error JSON).
- `initialize.instructions` for the meta-server: a terse version of the usage
  flow (list → read instructions/schemas → call; instanceId only needed when
  several instances of an app are connected).
- Known tradeoff, accepted: `call` takes an opaque payload — agents read
  schemas via `get_tools` instead of getting typed MCP tools. The per-instance
  MCP endpoints remain for clients that want typed tools for one app.
- Implementation note: keep it in `server.rs`/`registry.rs` only (no Tauri
  deps) so the headless `cargo run --example serve` keeps working — that is
  also how this gets e2e-tested.

### 2. Port stability — make the registered URL trustworthy

Persistent registration makes `http://localhost:7421/mcp` a contract. Today
`bind_port()` silently scans upward if 7421 is taken, which would orphan the
registration on the next launch.

- On startup, if 7421 is taken, probe it: `GET /status` (or `/apps` and check
  `"service": "web-bridge"`). If it's another live bridge, don't scan — surface
  "already running" (and ideally focus the existing window; Tauri's
  single-instance plugin covers the common case of double-launching the app).
- Keep upward scanning only as an explicit dev fallback (flag or env var), not
  the default behavior. The SDK's `?bridgePort=`/`port` override already
  handles the dev case on the app side.

### 3. "Connect your agent" panel in the hub UI

The hub window is the onboarding surface: it knows the real port and what's
connected, and it renders exact setup for each client.

- New section in `web-bridge/src/App.tsx`:
  - Claude Code: show
    `claude mcp add -s user --transport http web-bridge http://localhost:<port>/mcp`
    with a copy button. If the `claude` CLI is on PATH (checked via a small
    Tauri command), show a **"Set up Claude Code"** button that runs the
    command; fall back to copy-paste otherwise.
  - Generic MCP clients: a collapsible block with the equivalent JSON config
    snippet (`.mcp.json` shape) for Cursor / VS Code / others — same URL.
  - Status feedback after the button run (success / CLI not found / error
    output).
- New Tauri commands in `lib.rs`: `check_claude_cli`, `register_claude_mcp`
  (spawn `claude mcp add …` via `std::process::Command`), `install_skill`
  (workstream 4). Trust boundary: nothing is written or registered without an
  explicit button click.

### 4. The general `web-bridge` skill

One skill that teaches the protocol, not any domain (~one page):

- Content: check `GET /apps` → read the app's `description`/`instructions`
  and `GET /{app}/tools` for schemas → call `POST /{app}/api/{tool}` (or the
  meta-MCP if registered) → error semantics: 404 nothing connected, 409
  ambiguous shorthand (pick an `instanceId` from the payload), 422 the app
  rejected the call (its error explains why), 503/504 app gone or slow.
- Lives in the repo as `skills/web-bridge/SKILL.md` (single source of truth),
  embedded into the hub binary via `include_str!` so the UI's **"Install agent
  skill"** button can write it to `~/.claude/skills/web-bridge/SKILL.md`.
- Audience: shell-capable agents without MCP configured, and as a fallback/
  complement for MCP clients. For registered MCP clients the skill is mostly
  redundant by design — that's fine.

### 5. Agent-facing usage hint in discovery

Cheap safety net for agents that stumble onto the port with no prior setup:

- Add a short `usage` string to the `/apps` / `/` document
  (`AppState::apps_json` in `registry.rs`): two or three sentences covering
  the call pattern and where to find schemas. Keep it terse — this document is
  read by agents, and it already carries per-app instructions.

## Sequencing

| Step | Depends on | Why this order |
| ---- | ---------- | -------------- |
| 1. Meta-MCP endpoint | — | Everything else points at it; testable headless today |
| 2. Discovery `usage` hint | — | Trivial, independent |
| 3. Port stability | — | Should land before anyone registers URLs persistently |
| 4. Skill file in repo | 1 (mentions the meta-MCP) | Content depends on final endpoint shape |
| 5. UI panel + Tauri commands | 1, 3, 4 | Renders the command, runs the CLI, installs the skill |

Verification stays e2e, per repo convention: `cargo run --example serve` +
`examples/todo.mjs`, then exercise `/mcp` with curl (initialize → tools/list →
`call` for `todo/addTask`), plus the degraded cases (no app connected, two
instances, app rejects the payload).

## Out of scope (deliberately)

- **Per-app skills** — duplicate what the protocol self-serves; drift.
- **Auto-modifying agent configs on install** without a click — invasive.
- **Auto-detecting installed agents** beyond `claude` on PATH.
- **SSE / `tools/list_changed`** — the meta-tool design (`list_apps` /
  `get_tools` at call time) makes a static meta-catalog sufficient; the bridge
  still never initiates messages.

## Open questions

- Meta-tool naming: `list_apps` / `get_tools` / `call` vs. more explicit
  names (`bridge_list_apps`, …) to avoid collisions in clients that merge
  multiple servers' tools into one namespace.
- Should `register_claude_mcp` default to `-s user` scope (once per machine)?
  Current assumption: yes.
- Skill install locations for non-Claude agents (Cursor rules, etc.) — start
  with Claude Code only, add others on demand.
- Whether the panel should also offer per-app `claude mcp add` commands for
  users who want typed tools for one specific app (probably yes, behind a
  "per-app" expander, using the `/{app}/mcp` shorthand URL).
