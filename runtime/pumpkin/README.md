# Hosted Pumpkin runtime

The upstream Pumpkin binary does not contain the host-control interface required by this worker. For local development, build the pinned source and checked-in integration natively with `tools/build_pumpkin_native.sh`; this is the recommended path on macOS and does not require Docker. The isolated Linux image can be built with:

```sh
docker build -f runtime/pumpkin/Dockerfile -t melon-pumpkin:host-control .
```

Set `PUMPKIN_IMAGE=melon-pumpkin:host-control` on every worker. The image runs as UID/GID 2613; the worker sets data-volume ownership accordingly. DockerRuntime supplies the Unix socket path and authentication token. Tokens must contain at least 32 bytes. The worker requires Linux for the quota filesystem.

The interface provides readiness/player count, player hostname/edition, messages, native transfers, kicks, completed saves, and graceful shutdown. Pumpkin has no Postgres or scheduling dependency.

The runtime pins `caf954d17043e1f618f4afe254cbcd479492d80b`, the final revision before the NetherNet transport change. It reports Java 26.2 and Bedrock 1.26.40/protocol 2168, with normal UDP/RakNet. The local native lifecycle suite exercises real Java and Bedrock client logins, waiting sessions, transfers, and world recovery; see `PROGRESS.md` for the current evidence and the scope of its disposable test identities.

The integration patch also makes the save RPC await world/player/advancement/entity/block-entity/POI serialization and chunk/entity region writes. Watched regions are flushed rather than retained only in memory. Entity records are replaced for live chunks and deduplicated by UUID for dormant chunks, so repeated periodic saves do not append duplicate entities. Failed region writes restore dirty flags for retry. The native cross-worker recovery suite checks changed world blocks and player inventory after restart; its result is recorded in `PROGRESS.md`.

Pumpkin and the integration source are distributed under GPL-3.0; the license is included. The exact base source is available from Pumpkin-MC/Pumpkin at the pinned revision; the complete integration modifications are checked in here. Image consumers must retain the corresponding source and license when distributing binaries.

The container build fixes Rust to 1.98.1, uses one build job, disables upstream whole-program LTO, and uses sixteen code-generation units to bound compilation memory inside local Docker Desktop. BuildKit caches dependencies and artifacts. This changes build resource use, not the host-control protocol.
