# Control-plane worker supervision

The control plane exposes HTTP routes and runs durable coordination loops in the
same binary. Those responsibilities have separate internal seams:

- `start-queue` drains server wake requests.
- `worker-maintenance` assigns/requeues worker operations and performs object
  garbage collection.
- `ci-preview-maintenance` expires preview metadata, rehomes stale runtimes,
  requests checkpoints, and deletes expired artifacts.
- `ci-poller:<repository-id>` polls source control and drives the durable CI
  build queue.
- `ci-github-webhook` is optional and drains signed webhook deliveries.

`control_plane::supervision::Supervisor` owns every long-lived task's join
handle. A retryable failure updates the worker state and backs off; a fatal
failure or panic is returned to `main`, which shuts down the HTTP server rather
than leaving a process that reports healthy while coordination has stopped.
`/healthz` reports liveness of required workers and `/readyz` stays unavailable
until each required worker has completed one successful cycle. The API only
reads the registry; it does not own or spawn coordination tasks.

This is an intentional internal boundary, not yet a multi-process split. The
next step for independent scaling is to move the CI poller/build scheduler and
preview-runtime maintenance into a dedicated coordinator process. Their
leases and durable Postgres queues already provide the handoff seam; the
current supervisor keeps failures visible while that deployment split is
implemented.
