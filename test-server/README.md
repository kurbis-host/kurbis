# Pumpkin Vibe Test Server

Local development harness where a Pumpkin operator can ask Codex CLI to create or edit Rust/WASM Pumpkin plugins from Minecraft chat commands.

Requires Rust 1.96+, Git, curl, Python 3, and an authenticated Codex CLI on PATH.

Setup (from test-server/):

    ./scripts/bootstrap.sh
    ./scripts/run.sh

The server listens on 127.0.0.1:25565 in Java offline mode and runs the official release binary pinned in pumpkin.release, downloaded from Pumpkin-MC/Pumpkin and verified against pumpkin.checksums.sha256. Bootstrap fetches matching, unmodified source at pumpkin.revision solely to compile plugins against its SDK. It never builds the Pumpkin server.

From the Pumpkin console, grant yourself operator status:

    op <minecraft-name>

Then in game:

    /vibe code hello-world add a /hello command that says hello to the sender
    /vibe status <job-uuid>
    /vibe list
    /vibe cancel <job-uuid>
    /vibe revert hello-world

Jobs run serially. Codex edits only runtime/workspace/plugins/<name>, builds wasm32-wasip2 inside its workspace-write sandbox, and the controller validates and atomically installs the resulting WASM into the live Pumpkin plugin directory. Successful jobs report installed. Stop Pumpkin with the console command stop, then run ./scripts/run.sh again to activate created, edited, or reverted plugins. Hot reload is disabled because this pinned stock Pumpkin retains command permissions across plugin unloads.

The bridge and controller communicate only through Pumpkin's private plugins/data/vibe-bridge directory. The bridge has no network capability.

Everything under runtime/ is disposable local state and ignored by Git. Each generated plugin has its own Git repository. Successfully installed revisions are committed and their WASM artifacts are archived so /vibe revert can restore the previous known-good revision without executing generated build logic outside the Codex sandbox.

The WASM bridge cannot spawn native processes. Run the companion vibe-controller on the server host with an authenticated Codex CLI available on PATH. All integration behavior lives in the bridge and controller; bootstrap downloads the official Pumpkin server binary without source patches.
