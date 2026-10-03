# Kurbis control plugin

The managed Pumpkin runtime installs this plugin when a hosted server starts.
It uses the existing Kurbis HTTP API with a capability scoped to that server.
The capability is provisioned by the worker for the current placement and copied
into the server's private plugin data; the plugin does not select its API scope.

Build the WASI component with the pinned Pumpkin plugin API:

```sh
cargo build --locked --manifest-path plugins/kurbis-control/Cargo.toml \
  --release --target wasm32-wasip2
```

Operators can open `/kurbis` for status, players, logs, activity, settings,
whitelist, snapshots, and server controls. Logs and activity open as readable
Minecraft books instead of a chest full of truncated item lore. Settings are
editable directly from the menu: arrows cycle enum and distance values,
toggle buttons update booleans, and the writable-book control edits the MOTD.
The command form supports the same views, settings and whitelist changes,
lifecycle actions with an explicit confirmation step, and a two-step snapshot
restore. Restoring from an online runtime now performs a clean stop and queues
the restore automatically; sleeping servers restore immediately. `/kurbis login`
prints a one-use **HTTPS** browser link for this server. The link expires after
15 minutes; its redeemed browser grant does not sign in to a Kurbis account and
remains active until revoked or the server is removed or suspended.

Docker workers use the plugin baked into the hosted Pumpkin image. Native
workers build it from this crate by default; `KURBIS_CONTROL_PLUGIN_WASM` can
point to another local build artifact.
