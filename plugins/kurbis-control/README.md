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
whitelist, snapshots, and server controls. The command form supports the same
views, settings and whitelist changes, lifecycle actions with an explicit
confirmation step, and a two-step snapshot restore. `/kurbis login` prints a
one-use browser link for this server. The link expires after 15 minutes; its
redeemed browser session lasts one hour and does not sign in to a Kurbis account.

Docker workers use the plugin baked into the hosted Pumpkin image. Native
workers build it from this crate by default; `KURBIS_CONTROL_PLUGIN_WASM` can
point to another local build artifact.
