# Self-hosting

The root launcher starts the full service set in a tmux dashboard. The first
run creates a private `.env`; set the public panel URL, Minecraft root domain,
worker's reachable IP, and Google/Discord OAuth credentials in that file.
Register these callback URLs with both providers:

```text
https://<public-panel-origin>/auth/google/callback
https://<public-panel-origin>/auth/discord/callback
```

Choose one deployment mode:

```sh
./start.sh native debug    # host processes, fast Rust and Next.js iteration
./start.sh native release  # host processes, optimized Rust and production panel
./start.sh docker          # Compose services plus the host Docker worker
```

Native mode needs Rust 1.98.1, Git, CMake, Node.js/pnpm, tmux, and a local
PostgreSQL server. If `DATABASE_URL_NATIVE` cannot connect, the launcher uses
the current operating-system PostgreSQL role to create or update the configured
database role and create the database. That local role must have PostgreSQL
administrator permissions; remote PostgreSQL databases must be provisioned
separately. Native debug accepts an HTTP loopback `PUBLIC_BASE_URL`; native
release and Docker require HTTPS. Native mode does not use Docker. Docker mode
builds the pinned Pumpkin images and application images, starts PostgreSQL,
control plane, gateway, waiting runtime, and panel in Compose, then runs the
worker on the Linux host. The worker stays on the host because it controls the
host Docker engine and worker storage mounts. Docker mode needs Linux, Docker
Engine with Compose v2, Rust/Cargo, and tmux; use native mode on macOS. The
first Pumpkin image build takes substantially longer than later starts.

For native debug with `ROOT_DOMAIN=localhost`, the launcher creates a local
self-signed Bedrock TLS certificate when certificate paths are not configured.
Clients will need to trust that certificate to validate the local server. For
other hostnames, configure `BEDROCK_TLS_CERT_FILE` and
`BEDROCK_TLS_KEY_FILE` with a certificate trusted for the root domain and its
server subdomains.

The Docker Compose deployment publishes the panel on `PANEL_PORT`, Java on
`JAVA_PORT`, and Bedrock TCP and UDP on `BEDROCK_PORT`. For Docker deployments,
`BEDROCK_TLS_HOST_DIRECTORY` must point to a host directory containing
`fullchain.pem` and `privkey.pem`; Compose mounts that directory read-only at
`/run/kurbis-tls` and configures the gateway to use those files. Provide a
certificate trusted for `ROOT_DOMAIN` and its server subdomains. The private key
must be readable by the gateway's container UID/GID `2613:2613` without granting
world access (for example, a root-owned key with group `2613`, mode `0640`, and
traversable parent directories or an equivalent ACL). Do not make the key
world-readable to work around container permissions. The Docker gateway does not
generate production certificates.

Put an HTTPS reverse proxy in front of the panel port and point `PUBLIC_BASE_URL`
at that HTTPS origin. Open
the worker's configured TCP/UDP port range (`WORKER_PORT_MIN` through
`WORKER_PORT_MAX`) as well as the gateway ports. For enforced Linux filesystem
quotas, set `WORKER_STORAGE_MODE=loopback_ext4` and run the worker with the
permissions needed to mount ext4 loop devices; `directory` mode has no hard
disk quota. Docker workers require `WORKER_GATEWAY_ADDRESS` to equal the source IP the
Gateway uses to reach the worker and require `WORKER_GATEWAY_BEDROCK_IP` plus
`WORKER_GATEWAY_BEDROCK_PORT` to match the public shared Bedrock Gateway endpoint.
Each rootless Pumpkin remains on a per-runtime internal network; a bounded,
fixed-destination relay publishes only its Java, NetherNet UDP, and signaling
ports on separate per-runtime networks. Host nftables admits only the Gateway
source on those leased ports. NetherNet SDP advertises the shared Gateway
endpoint rather than a worker port, so clients cannot bypass the Gateway ACL;
missing nftables privileges fail worker startup before it acquires assignments.
For remote Gateways, configure the actual routed source address with
`WORKER_GATEWAY_ADDRESS` (network ACLs must preserve or predictably SNAT that
source). Local Compose defaults it to `GATEWAY_IP`. A matching Java probe is
also published on loopback for worker-local readiness checks; it is not
reachable from remote hosts. The worker must have host `CAP_NET_ADMIN` for the
dedicated nftables chain; this firewall privilege is separate from Docker,
which must still run rootless under a non-root account.

Managed Pumpkin plugin API traffic uses a per-server, per-generation sidecar
proxy. The server container remains on its own Docker `internal` network with
IPv6 and masquerading disabled; that network contains Pumpkin, its API proxy,
and its fixed-backend ingress relay. The relay's own OUTPUT firewall permits
only the exact Pumpkin IPv4 address and Java/Bedrock backend ports. Only the proxy also joins `WORKER_RUNTIME_API_EGRESS_NETWORK`. It carries
no worker, gateway, database, or object-store credentials: it pins the request
to the configured `WORKER_RUNTIME_API_PROXY_UPSTREAM`, checks the presented
server/preview capability against its hash, and allows only the managed plugin's
own server or CI-preview routes and methods. The upstream must use HTTPS. For
local Compose only, the checked-in `.env.example` opts into private-network HTTP
and uses `http://control-plane:8080` on the dedicated internal
`kurbis-hoster_runtime-api-egress` network shared with the control plane and
managed proxy sidecars (and no panel, gateway, or database service). Remote
workers may use an IPv4 bridge with external routing for the control-plane
origin, but must use HTTPS and apply host/cloud egress rules that allow only
that origin; the proxy itself still pins every HTTP request to that single
configured origin.
Redirects are never followed; bodies are bounded; GETs retry once only for a
connection error or timeout, while mutations are never retried. An unhealthy
proxy fails runtime startup closed. The sidecar's port is never host-published,
so gateway-only Minecraft ingress remains unchanged.
The starter `.env` uses local snapshot storage under `data/objects`;
for production durability, set `OBJECT_STORE_TYPE=s3` and configure the `S3_*`
values. Set `DOCKER_SUBNET`, `WAITING_IP`, and `GATEWAY_IP` if the default
Compose subnet conflicts with an existing network.

Use `./start.sh attach` to reopen the dashboard and `./start.sh stop` to stop
the dashboard and Compose services. In tmux, use the mouse or `Ctrl-b` followed
by an arrow key to select a process pane; `Ctrl-b z` zooms the selected pane,
and `Ctrl-b d` detaches. Persistent PostgreSQL data is kept when stopping
Compose. Stop hosted worlds from the panel before shutting down their worker.

## Operator dashboard

Accounts listed in [`ADMIN_IDENTITIES`](configuration.md) can open `/admin`.
It is read-only: an admin sees everything and can change nothing through it.
Its API lives under `/api/v1/admin/*` and answers 404 to everyone else.

- `/admin` shows health: background tasks, workers, the start queue, failed
  and stuck servers, operations and the GitHub rate limit. Below are fleet
  charts for the last hour, day, week and month, and storage use.
- `/admin/servers`, `/admin/accounts` and `/admin/workers` list every server,
  account and worker. `/admin/accounts/<id>` shows one account with its
  sign-ins and servers.
- `/admin/failures` collects failed operations; `/admin/ci` the Pumpkin
  preview builds; `/admin/ai` assistant use and spend; `/admin/growth` the
  daily totals.
- An admin can open any server's panel at `/manage/<id>` and read it. Its
  Lifecycle tab, shown only to admins, adds the owner, placement, operations,
  snapshots and usage samples.

The dashboard keeps its own history, so retention is:

| Data | Kept |
| --- | --- |
| `fleet_stats`, one sample of the whole service a minute | 30 days |
| `server_runtime_samples`, one sample of each running server a minute | 7 days |
| `daily_stats`, one row a UTC day: signups, servers created, wakes, servers woken, peak servers and players, AI spend | for good |
| `server_logs`, console lines | 14 days |
| `server_events`, server activity | 7 days |

Migration 0023 fills `daily_stats` from the rows that already exist. The
control plane writes the minute samples and prunes them. The
`worker-maintenance` task deletes old console lines and events in bounded
batches.

## How kurbis.host releases

For production releases, run `python3 scripts/release.py` from a clean checkout
exactly matching `origin/main`; [`deployment/production-rollout.md`](../deployment/production-rollout.md)
describes what it does. It compiles Pumpkin on the remote CI builder, deploys
the Compose stack through Dokploy, and then moves the separate systemd-managed
host worker onto the release, restoring the previous worker if the new one
does not come up. Set `TEST_DATABASE_URL` to a disposable PostgreSQL database
for the release gate. For stranded CI coordination rows, use
the bounded, dry-run-first procedure in [`deployment/ci-cleanup.md`](../deployment/ci-cleanup.md);
never reset production data with broad SQL or Compose volume deletion.

See [configuration](configuration.md) for OAuth, the AI assistant and object storage, and [Pumpkin previews](pumpkin-previews.md) for the optional CI preview service.
