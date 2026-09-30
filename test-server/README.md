# Pumpkin Vibe Test Server

Local development harness where a Pumpkin operator can ask Codex CLI to create or edit Rust/WASM Pumpkin plugins from Minecraft chat commands.

Setup:

    ./scripts/bootstrap.sh
    ./scripts/run.sh

The server listens on 127.0.0.1:25565 in Java offline mode and uses the Pumpkin revision pinned in pumpkin.revision. Bootstrap creates a separate clean Pumpkin checkout under runtime/ and never modifies ../../Pumpkin. It applies patches/pumpkin-hot-reload.patch only to that disposable checkout so atomic plugin replacement is recognized consistently and plugin-owned permission registrations are cleaned up during hot reload.

From the Pumpkin console, grant yourself operator status:

    op <minecraft-name>

Then in game:

    /vibe code hello-world add a /hello command that says hello to the sender
    /vibe status <job-uuid>
    /vibe list
    /vibe cancel <job-uuid>
    /vibe revert hello-world

Jobs run serially. Codex edits only runtime/workspace/plugins/<name>, builds wasm32-wasip2 inside its workspace-write sandbox, and the controller validates and atomically installs the resulting WASM into the live Pumpkin plugin directory. Pumpkin hot reloads successful deployments.

The bridge and controller communicate only through Pumpkin's private plugins/data/vibe-bridge directory. The bridge has no network capability.

Everything under runtime/ is disposable local state and ignored by Git. Each generated plugin has its own Git repository. Successfully deployed revisions are committed and their WASM artifacts are archived so /vibe revert can restore the previous known-good revision without executing generated build logic outside the Codex sandbox.
