# Pumpkin Vibe Test Server

An operator-only Pumpkin plugin for creating other Rust/WASM plugins with Codex CLI. `/vibe` opens a six-row chest workshop with settings, installed plugins, conversations, jobs, and detailed logs.

Requires Rust 1.96+, Git, curl, Python 3, and an authenticated Codex CLI on PATH.

From `test-server/`:

```sh
./scripts/bootstrap.sh
./scripts/run.sh
```

The server listens on `127.0.0.1:25565` in Java offline mode. It runs the **official release binary** pinned in `pumpkin.release`, downloaded from Pumpkin-MC/Pumpkin and verified against `pumpkin.checksums.sha256`. The matching, unmodified checkout at `pumpkin.revision` supplies the plugin SDK only. Bootstrap never builds or patches the Pumpkin server.

Grant yourself operator status from the Pumpkin console with `op <minecraft-name>`, then run `/vibe` in game.

## Chest workshop

- **Installed plugins:** inspect revisions and sizes; click a plugin to reload it or restore the preceding archived revision.
- **Conversations:** browse saved prompts and their responses/tool events; continue the same Codex thread.
- **Jobs & live logs:** browse every event with previous/next arrows, refresh status, watch privately in chat, or cancel. Click an event to read its complete message and raw JSON in numbered parts.
- **Create or edit a plugin:** click, close the chest, and type `plugin-name your prompt` in chat.
- **Ask Codex:** use the same input flow for a discussion in the plugin's workspace. These turns run in Codex's read-only sandbox and do not install an artifact.
- **Settings:** enter a model ID (or `default` for your CLI configuration), cycle reasoning effort and timeout, toggle live chat progress, or restore defaults. Defaults: CLI model, medium effort, 15-minute timeout, chat progress on. Model/effort availability depends on your configured Codex provider.

Menu input consumes your next chat message privately. Type `cancel` to abort; prompts expire after two minutes. Permissions are checked again on clicks and submission, including if you lose OP while a menu is open. Closing or leaving cleans up menu state; leaving also clears pending input and watches. Menu items cannot be moved into your inventory. The workshop works in spectator mode without changing your gamemode; it handles spectator clicks through Pumpkin's packet event API because the stock server skips inventory click events for spectators.

Settings persist on disk. Execution settings apply when the next queued job starts. Live chat progress can be toggled immediately; full disk logging stays enabled. Timeouts cycle through 5/15/30/60 minutes; the command interface permits 30–3600 seconds. The controller's optional `--timeout-seconds` sets an upper limit, defaulting to one hour.

## Commands

```text
/vibe
/vibe settings
/vibe jobs
/vibe sessions
/vibe code hello-world add a /hello command that greets the sender
/vibe chat hello-world explain how the greeting command works
/vibe continue <session-uuid> make the greeting configurable
/vibe status <job-uuid>
/vibe log <job-uuid> [page]
/vibe list
/vibe cancel <job-uuid>
/vibe revert hello-world
/vibe reload hello-world
/vibe set model default
/vibe set reasoning high
/vibe set timeout 1800
/vibe set verbose true
```

Log pages are zero-based. Console users get text output for logs, sessions, status, and installed plugins. Each new code/chat request starts a conversation; continuing retains its workspace and mode. Code conversations continue generating/installing plugins; chat conversations remain discussions. A conversation becomes resumable once a turn has recorded its actual Codex thread ID. Codex's local session storage must remain available to the controller account.

## Logs and installation

Jobs run serially in creation order. Each plugin has its own Git workspace at `runtime/workspace/plugins/<name>`. Code turns use Codex's `workspace-write` sandbox to edit and build `wasm32-wasip2`. The controller validates and atomically installs the resulting WASM, commits the successful source revision, and archives the artifact for restoration. Failed or cancelled turns reset uncommitted source changes and preserve the previously installed artifact. Queued cancellations never start Codex; active Codex cancellation, timeout, and graceful controller shutdown terminate its process group. Interrupted jobs are marked failed on restart, their source changes reset, and their recorded Codex thread remains available for continuation. Requests are never silently replayed.

**Installed and restored plugins reload automatically without disconnecting players.** `scripts/run.sh` runs the official binary through a companion console runner. The controller waits for confirmed unload/load results and records them in the job log; initialization failure restores and reactivates the previous artifact. Use `/vibe reload <plugin>` or **Installed plugins → plugin → Reload plugin** to reload the current artifact without a Codex turn.

Start the server with `scripts/run.sh` to enable this integration. If the controller runs alone, builds are staged and report `reload pending`; start the runner and use `/vibe reload <plugin>`. The running `vibe-bridge` is protected from self-reload. Stop/start once when upgrading the bridge or switching an existing server to the companion runner; generated plugin changes then activate live.

The stock file watcher stays disabled: it misses atomic rename installations on the tested release. The native load command also rejects paths containing `/`, so the runner uses a temporary hard link in the server directory and removes it after loading. No Pumpkin changes are needed.

Generated plugins use `vibe-plugin-support::register_permission` for repeatable initialization. The stock permission registry survives unloads; the helper accepts only the exact duplicate-node error and preserves all other failures. Existing permission defaults remain unchanged until a server restart. Old generated plugins that register permissions directly must adopt the helper; Codex continuation prompts include this instruction.

The bridge and controller communicate through `runtime/server/plugins/data/vibe-bridge/`:

| File | Contents |
| --- | --- |
| `settings.json` | Persisted workshop settings |
| `sessions.json` | Conversation IDs, actual Codex thread IDs, prompts, and job links |
| `results/<job>.json` | Job state and installation/failure message |
| `events/<job>.jsonl` | Every exposed Codex JSON event, non-JSON stdout, stderr, user prompt, and controller stage; complete output without truncation |
| `progress/<job>.json` | Latest 100 event previews for live chat |
| `plugins.json` | Installed plugin revisions and artifact sizes |
| `activation/` | Console runner heartbeat and temporary reload handoff |

The controller also prints each event as it arrives. Commands, tool output, file-change events, agent messages, errors, and token usage are retained. Logging covers what Codex CLI emits; it cannot expose private model reasoning or events the CLI does not emit. Chest event previews and live chat abbreviate long output; event detail pages and JSONL files retain the complete data. Refresh the chest to load newly arrived events. If chat falls behind the preview window, it points you to the full job log.

The WASM bridge has only Pumpkin's private data filesystem permission and no networking or native process capability. Run the companion `vibe-controller` on the same host, with Codex authenticated under its OS account. Everything under `runtime/` is local state ignored by Git; deleting it also deletes plugins and conversation metadata.

## Verification

After bootstrap:

```sh
cargo test --manifest-path Cargo.toml --workspace
cargo test --manifest-path vibe-bridge/Cargo.toml --lib
cargo clippy --manifest-path Cargo.toml --all-targets -- -D warnings
cargo clippy --manifest-path vibe-bridge/Cargo.toml --target wasm32-wasip2 -- -D warnings
python3 tests/dashboard_e2e.py
```

The end-to-end test runs the official binary and bridge in an isolated temporary runtime with real protocol clients and a deterministic Codex event fixture. It checks permissions, chest navigation in survival and spectator mode, gamemode changes with an open menu, stale-window rejection, drag suppression, settings, private prompts, log browsing, and continuation across controller restart. Backend tests cover installation/restoration, invalid artifacts, complete large outputs, thread mismatch, FIFO ordering, cancellation, timeout, and settings validation. Reload coverage compiles real WASM revisions, verifies changed command responses on the same connections, exercises the chest Reload button, restores the previous revision, and rejects failed initialization, invalid permission namespaces, protected names, missing plugins, and non-OP requests. It checks rollback bytes, revision metadata, ordered logs, and temporary link cleanup. Bridge unit tests cover truncated and invalid click headers. The Codex argument regression keeps network and reasoning overrides together before `exec`, preventing exec-local configuration from silently disabling network access.
