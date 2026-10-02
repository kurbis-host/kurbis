# Runtime verification

Run the complete acceptance harness against local Docker Desktop. It builds
temporary hoster, Pumpkin, offline-authentication, and verification images from
this checkout, then runs a privileged Linux container with its own isolated
Docker daemon. The wrapper rejects nonlocal Docker endpoints and gives the
outer container and images a unique run tag. Inner container names use unique
test IDs.

```sh
sh tools/verify_local_runtime.sh
```

The wrapper requires macOS with Docker Desktop running. It builds the images
it needs itself; do not substitute pre-existing application images. It passes
no host Docker socket or existing Docker data volume into the verifier. The
outer verifier container is limited to 6 GiB RAM and two CPUs; its inner engine
uses VFS for the nested Docker workloads. The wrapper removes only its
run-tagged container, images, and anonymous volumes. The inner harness removes
the test containers it creates. The output directory and evidence remain under
`output/runtime/melon-local-verification-<uuid>/`; the wrapper prints the
exact directory when the run finishes.

Each run writes `result.json`, verifier and daemon logs, waiting and backend
logs, and individual Java protocol proofs. A runtime result is complete only
when the command exits successfully and `result.json` contains
`"passed": true`. Image compilation, health checks, or a partial scenario do
not count as a passing run.

The harness starts fresh Postgres, the actual control plane and workers, the
offline-authentication Pumpkin image, and the Bedrock gateway. It exercises
the generated Rust/OpenAPI lifecycle contract at
`POST /api/v1/servers/{server_id}/commands`; the request bodies come from the
`api-openapi` binary built into the verifier image. The test provisioner gets
its operator database connection as an explicit provisioning input; the emitted
worker bundle and
worker process are checked for database, object-store, cloud, OAuth, and
global internal credentials. Worker health is proven through the control
plane heartbeat and fenced API lease. A killed worker's lease remains fenced
until its 30-second expiry, and the recovery scenarios wait for that expiry
before starting the same worker identity again.

Runtime scenarios cover hard 2 GiB memory and 5 GiB disk limits, zero swap,
real Java 26.3 joins and commands, hostname routing, capacity queueing,
owner restart, scoped world reset with recovery snapshots, and worker-generated
world export with an authenticated ZIP download containing player data.
Snapshots are exercised during object-store outage and recovery, process loss,
stale generation rejection, cross-worker restore, and graceful save, shutdown,
and resume. The Bedrock checks exercise trusted TLS and hostname validation,
status and SDP signaling, malformed-request rejection, and UDP relay isolation.
They do not use a live Bedrock client.

The harness also checks the five-minute idle timer across a player rejoin,
one-use 15-minute server login links, account-plus-scope server-list union,
scoped access that remains valid until explicitly revoked, scoped reset and
delete limited to the bound server, and operator-only Kurbis plugin commands.
Scoped browser grants have no time expiry; worker API leases are separate and
continue to expire after worker loss.

The verifier retains a native development mode for local lifecycle debugging,
but native processes cannot prove Linux cgroup or filesystem quota enforcement
and are not a substitute for the Docker acceptance run. Syntax checks and
startup-only runs are not acceptance evidence.
