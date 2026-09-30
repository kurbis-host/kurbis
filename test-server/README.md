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

- **Installed plugins:** inspect revisions and sizes; restore the preceding archived revision.
- **Conversations:** browse saved prompts and their responses/tool events; continue the same Codex thread.
- **Jobs & live logs:** browse every event with previous/next arrows, refresh status, watch privately in chat, or cancel. Click an event to read its complete message and raw JSON in numbered parts.
- **Create or edit a plugin:** click, close the chest, and type `plugin-name your prompt` in chat.
- **Ask Codex:** use the same input flow for a discussion in the plugin's workspace. These turns run in Codex's read-only sandbox and do not install an artifact.
- **Settings:** enter a model ID (or `default` for your CLI configuration), cycle reasoning effort and timeout, toggle live chat progress, or restore defaults. Defaults: CLI model, medium effort, 15-minute timeout, chat progress on. Model/effort availability depends on your configured Codex provider.

Menu input consumes your next chat message privately. Type `cancel` to abort; prompts expire after two minutes. Permissions are checked again on clicks and submission, including if you lose OP while a menu is open. Closing or leaving cleans up menu state; leaving also clears pending input and watches. Menu items cannot be moved into your inventory.

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
/vibe set model default
/vibe set reasoning high
/vibe set timeout 1800
/vibe set verbose true
```

Log pages are zero-based. Console users get text output for logs, sessions, status, and installed plugins. Each new code/chat request starts a conversation; continuing retains its workspace and mode. Code conversations continue generating/installing plugins; chat conversations remain discussions. A conversation becomes resumable once a turn has recorded its actual Codex thread ID. Codex's local session storage must remain available to the controller account.

## Logs and installation

Jobs run serially in creation order. Each plugin has its own Git workspace at `runtime/workspace/plugins/<name>`. Code turns use Codex's `workspace-write` sandbox to edit and build `wasm32-wasip2`. The controller validates and atomically installs the resulting WASM, commits the successful source revision, and archives the artifact for restoration. Failed or cancelled turns reset uncommitted source changes and preserve the previously installed artifact. Queued cancellations never start Codex; active cancellation, timeout, and graceful controller shutdown terminate its process group. Interrupted jobs are marked failed on restart, their source changes reset, and their recorded Codex thread remains available for continuation. Requests are never silently replayed.

**Restart Pumpkin to activate installed or restored plugins.** Stop it with console `stop`, then run `./scripts/run.sh`. Hot reload is disabled because this stock release retains command permissions across plugin unloads.

The bridge and controller communicate through `runtime/server/plugins/data/vibe-bridge/`:

| File | Contents |
| --- | --- |
| `settings.json` | Persisted workshop settings |
| `sessions.json` | Conversation IDs, actual Codex thread IDs, prompts, and job links |
| `results/<job>.json` | Job state and installation/failure message |
| `events/<job>.jsonl` | Every exposed Codex JSON event, non-JSON stdout, stderr, user prompt, and controller stage; complete output without truncation |
| `progress/<job>.json` | Latest 100 event previews for live chat |
| `plugins.json` | Installed plugin revisions and artifact sizes |

The controller also prints each event as it arrives. Commands, tool output, file-change events, agent messages, errors, and token usage are retained. Logging covers what Codex CLI emits; it cannot expose private model reasoning or events the CLI does not emit. Chest event previews and live chat abbreviate long output; event detail pages and JSONL files retain the complete data. Refresh the chest to load newly arrived events. If chat falls behind the preview window, it points you to the full job log.

The WASM bridge has only Pumpkin's private data filesystem permission and no networking or native process capability. Run the companion `vibe-controller` on the same host, with Codex authenticated under its OS account. Everything under `runtime/` is local state ignored by Git; deleting it also deletes plugins and conversation metadata.

## Verification

After bootstrap:

```sh
cargo test --manifest-path Cargo.toml --workspace
cargo clippy --manifest-path Cargo.toml --all-targets -- -D warnings
cargo clippy --manifest-path vibe-bridge/Cargo.toml --target wasm32-wasip2 -- -D warnings
python3 tests/dashboard_e2e.py
```

The end-to-end test runs the official binary and bridge in an isolated temporary runtime with real protocol clients and a deterministic Codex event fixture. It checks permissions, chest navigation, settings, private prompts, log browsing, and continuation across controller restart. Backend tests cover installation/restoration, invalid artifacts, complete large outputs, thread mismatch, FIFO ordering, cancellation, timeout, and settings validation.
