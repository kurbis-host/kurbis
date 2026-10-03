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
whitelist, snapshots, and server controls. All navigation and setting updates
occur in-place within the chest container without closing or reopening the screen,
preventing mouse cursor resets. Logs and activity open in paged in-menu lists
rather than written books or inventory items. Settings are editable directly from
the menu: difficulty and game mode use in-menu option selectors with predefined values,
render distance uses arrows, booleans toggle on click, and the MOTD control uses
a clickable chat prompt or Bedrock modal form without inventory pollution. Settings
may be edited online or while sleeping; runtime configuration values are applied
on the next server start. Whitelist entries can be added by name, removed by
clicking an entry with confirmation, and enabled or disabled with one click; changes
are synchronized to the online runtime.
The command form supports the same views, settings and whitelist changes,
lifecycle actions with an explicit confirmation step, and a two-step snapshot
restore. Restoring from an online runtime performs a clean stop and queues
the restore automatically; sleeping servers restore immediately. `/kurbis login`
prints a one-use browser link for this server using the configured public URL scheme. The link expires after
15 minutes; its redeemed browser grant does not sign in to a Kurbis account and
remains active until revoked or the server is removed or suspended.

Docker workers use the plugin baked into the hosted Pumpkin image. Native
workers build it from this crate by default; `KURBIS_CONTROL_PLUGIN_WASM` can
point to another local build artifact.
