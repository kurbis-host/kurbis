# Kurbis AI Test Server

Kurbis AI is the operator-only Pumpkin plugin in `plugins/kurbis-ai`. It combines the plugin workshop (create, edit, build, install, reload, and restore Rust/WASM plugins) with an AI assistant that can inspect and immediately control the live server through a per-job stdio MCP server. The Minecraft command is `/ai`.

Requires Rust 1.96+, Git, curl, Python 3, and Codex CLI on PATH. On first use, run `/ai login` in game to start the private device-code flow for the single shared server-owner Codex profile. You can also authenticate the account directly on the controller host. The controller keeps that profile under `runtime/codex-home`; do not share that directory or its credentials.

From `test-server/`:

```sh
./scripts/bootstrap.sh
./scripts/run.sh
```

The server listens on `127.0.0.1:25565` in Java offline mode. It runs the **official release binary** pinned in `pumpkin.release`, downloaded from Pumpkin-MC/Pumpkin and verified against `pumpkin.checksums.sha256`. The matching, unmodified checkout at `pumpkin.revision` supplies the plugin SDK only. Bootstrap never builds or patches the Pumpkin server. It builds the canonical plugin from `../plugins/kurbis-ai/` and removes only the obsolete Vibe Bridge WASM/data; generated plugin workspaces and archived artifacts are preserved.

Grant operator status from the Pumpkin console with `op <minecraft-name>`, then run `/ai` in game. Kurbis AI checks the plugin permission both when a command opens the feature and again when menus, private prompts, login deliveries, and MCP actions are processed. Each server-control action is bound to the initiating operator UUID and is re-authorized against their current online OP status. Actions execute immediately without confirmation.

## Chest workshop and AI

- **Installed plugins:** inspect revisions and sizes; reload a plugin or restore its preceding archived revision.
- **Conversations:** browse saved prompts and responses/tool events, then continue a Codex thread.
- **Jobs & logs:** inspect live status, read complete event details, watch progress privately, or cancel a job.
- **Create or edit a plugin:** choose the workshop action and enter `plugin-name your prompt` in chat. Code jobs build and install the result.
- **Ask Kurbis AI:** ask a question or request a server action. Each job receives only its own local stdio MCP connection with typed server/player/world/entity inspection, direct entity spawning, and vanilla and installed-plugin command dispatch.
- **Link Codex account:** use `/ai login` to start device authentication for the shared owner profile. The one-time URL and code are sent only to the player who started login. They are never sent to other players or included in job/server logs.
- **Settings:** choose model, reasoning, timeout, live progress, or restore defaults. Defaults are Codex CLI's configured model, medium reasoning, 15 minutes, and progress enabled.

Menu input consumes the next chat message privately. Type `cancel` to abort; prompts expire after two minutes. Permissions are checked on clicks and submission, including after de-op. Closing or leaving clears menu state, pending input, and watches. In spectator mode, use the `/ai` command subcommands directly. The pinned official Pumpkin build skips inventory clicks for spectators, and its generic packet-received plugin event currently panics while deserializing ordinary client packets; Kurbis AI therefore intentionally does not subscribe to that unsafe event path. This preserves server stability and keeps command-based AI/plugin actions available.

Kurbis AI calls Codex with the shared controller-owned login profile copied into an isolated per-session `CODEX_HOME`, ignores ambient user configuration, and injects only the current job's required MCP server configuration. Codex sessions stay persistent for conversation continuation. The local MCP server serializes actions, limits calls to four per second and 128 per job, bounds request/result sizes, correlates every response by UUID, and rejects stale jobs or operators who are no longer authorized. It has no host-shell, filesystem, provider-credential, or arbitrary plugin-IPC tools. Server-command dispatch intentionally permits broad control of the current Pumpkin server and its installed plugins, as requested; plugin-specific actions should be exposed through their own commands or typed MCP tools.

## Commands

```text
/ai
/ai settings
/ai jobs
/ai sessions
/ai code hello-world add a /hello command that greets the sender
/ai ask spawn a pig near me
/ai login
/ai continue <session-uuid> make the greeting configurable
/ai status <job-uuid>
/ai log <job-uuid> [page]
/ai list
/ai cancel <job-uuid>
/ai revert hello-world
/ai reload hello-world
/ai set model default
/ai set reasoning high
/ai set timeout 1800
/ai set verbose true
```

Log pages are zero-based. Plugin code jobs and AI server-control jobs have separate workspaces/modes and sessions. The server-control mode is read-only with respect to its workspace; all live mutations go through the server's typed MCP actions.

## Installation, state, and hosting boundary

Jobs run serially in creation order. Each generated plugin has a Git workspace under `runtime/workspace/plugins/<name>`. Successful code turns validate and atomically install WASM, commit the source revision, and archive the binary. Failed/cancelled turns roll back uncommitted changes. Running `scripts/run.sh` starts the official Pumpkin binary with the companion console runner, enabling confirmed live reload and rollback without disconnecting players. The Kurbis AI plugin itself is protected from reload; restart once after upgrading the controller/plugin.

The WASM plugin has only access to its own private data directory and no network or native-process capability. The native `kurbis-ai-controller` owns Codex, the device-auth profile, the per-job stdio MCP server, and the file-backed handoff. Durable workshop settings, sessions, jobs, and logs live under `runtime/server/plugins/data/kurbis-ai/`; MCP action results use separate transient subdirectories. A short-lived device-auth message is delivered through the plugin's private local data channel and consumed by Kurbis AI; the verification URL/code never enter job logs, controller logs, or broadcast chat. Everything under `runtime/` is ignored by Git. Deleting it also deletes generated plugins, artifacts, conversations, and the shared Codex login.

A hosted Kurbis AI gateway is intentionally not implemented in this phase. When added, it must keep provider credentials server-side, scope account/session/quota to the hosted server, enforce isolated concurrency/tool/token/cost budgets, and remain separate from MCP server-action authorization. Gateway model access must never confer server-control permission.

## Verification

From the repository root:

```sh
cargo test --manifest-path test-server/Cargo.toml --workspace
cargo clippy --manifest-path test-server/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path plugins/kurbis-ai/Cargo.toml
cargo clippy --manifest-path plugins/kurbis-ai/Cargo.toml --target wasm32-wasip2 -- -D warnings
python3 test-server/tests/dashboard_e2e.py
```

The end-to-end suite runs the official binary in isolated runtimes. It verifies `/ai` permission gates, chest navigation in survival, command-based spectator control, private prompts and login delivery, settings, Codex session continuation, direct MCP spawn/command behavior, action authorization/audit, live plugin reload, rollback, and failed plugin initialization. Controller/protocol tests cover MCP JSON-RPC initialization/tool discovery, strict request correlation, bounded parsing, device-code privacy, Codex argument isolation, sessions, cancellation, timeouts, WASM validation, and installation rollback.
