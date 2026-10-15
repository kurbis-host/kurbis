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
whitelist, snapshots, server controls and the AI assistant. All navigation and setting updates
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
When the whitelist turns someone away, the plugin reports them to the panel,
whose players page lists them with Let in and Dismiss (and, while the owner
has no linked Minecraft account, That's me, which whitelists that player and
makes them an operator). It also tells every operator online
in chat, and every player on the whitelist while the server's
`whitelist_by_members` setting is on (the default; the whitelist page of the
menu and the web panel turn it off). The message links to `/letin <name>`,
which asks for confirmation; `/letin <name> confirm` adds them through the
API, which checks the adder again and records who added them on the whitelist
entry and in the server's activity. Only players turned away in the last 15
minutes can be let in this way, and each one is announced at most once a
minute. Pumpkin checks the whitelist only for Java logins, so the plugin also
stops Bedrock players who are not on it. Adds from `/kurbis whitelist add` and
the menu record the operator who made them too.
The command form supports the same views, settings and whitelist changes,
lifecycle actions with an explicit confirmation step, and a two-step snapshot
restore. Restoring from an online runtime performs a clean stop and queues
the restore automatically; sleeping servers restore immediately. `/kurbis login`
prints a one-use browser link for this server using the configured public URL scheme. The link expires after
15 minutes; its redeemed browser grant does not sign in to a Kurbis account and
remains active until revoked or the server is removed or suspended.

Operators can also ask the Kurbis AI assistant to act on the server.
`/ai ask <question>` sends the question to the AI gateway on the control
plane; follow-ups within an hour continue the same conversation, and
`/ai new <question>` starts a fresh one. `/ai cancel` stops the running
request and `/ai status` shows the model and this month's spending against the
server and account budgets. While a request runs, the plugin syncs with the
gateway every half second: it runs the tools the model asks for (server
status, players, worlds, entities and server commands, executed as the asking
operator) and shows progress lines and the answer in that operator's chat. The
gateway records the cost of every model call and refuses new requests once a
budget is used up. `/ai` on its own opens the assistant's page in the chest
menu: the latest requests of every operator with their state and cost, Ask and
New conversation buttons that use the same chat prompt or Bedrock form as the
MOTD control, and this month's spending. A request opens to its tool calls with
their input and output, its whole answer or error (in chat on Java, in a form on
Bedrock) and, while it runs, a cancel button. Model, effort, the server budget
and the on/off switch are changed in the web panel; the menu's settings button
prints a login link.
CI preview servers do not offer `/ai`.

`/plugins` manages the server's plugins and replaces Pumpkin's built-in
listing, which shows only what is loaded. On its own it opens the plugins page
of the menu: every installed plugin with what the server does with it
(running, waiting to load, failed with Pumpkin's error, or turned off), a
plugin's page to turn it on or off, update it from the market or remove it
with confirmation, and the market: free plugins from market.pumpkinmc.org,
most downloaded first, with a search that uses the same chat prompt or Bedrock
form as the MOTD control. `/plugins market` opens the market and
`/plugins search <text>` searches it; from the console both print a list
whose `[Install]` buttons run `/plugins install <id>`.
`/plugins install <name|id>` installs a listing or updates it to the market's
current version, `/plugins enable|disable <name>` turns a plugin on or off and
`/plugins remove <name>` asks for `/plugins remove confirm <name>`. A running
server loads each change without a restart. Uploading a plugin needs the web
panel; the menu's upload button prints a login link. CI preview servers do not
offer `/plugins`.

Docker workers use the plugin baked into the hosted Pumpkin image. Native
workers build it from this crate by default; `KURBIS_CONTROL_PLUGIN_WASM` can
point to another local build artifact.
