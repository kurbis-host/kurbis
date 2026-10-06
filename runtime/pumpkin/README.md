# Hosted Pumpkin runtime

The hosted runtime builds Pumpkin directly on the local host with
`tools/build_pumpkin_native.sh`, which serializes Rust compilation behind the shared host-memory admission gate; local development does not use Docker. The
source pin is Pumpkin `0.2.0+26.3-26.51`, which speaks Java protocol 777 and
Bedrock 1.26.51/protocol 2193.

The checked-in integration adds the authenticated Unix host-control socket
used by workers for player state, messages, transfers, saves, whitelist and
operator updates, and shutdown. Snapshot saves flush watched chunk regions and
return write failures so the worker can avoid marking an incomplete recovery
point as complete. `tools/protocol/verify_java_263.py` exercises real protocol
777 logins, transfers, commands, inventory, and reconnect persistence.

Bedrock 1.26.51 uses NetherNet, whose signaling requests use HTTPS and exchange
complete SDP offers and answers. At the pinned Pumpkin revision, the HTTP
signaling listener was a plain Axum service on the same port as UDP status/ICE;
it could not complete the TLS handshake required by current clients. Kurbis now
terminates TLS on the public gateway and proxies only `/v1/join` signaling to
Pumpkin over its private signal interface. The checked-in
`nethernet-gateway.patch` separates that TCP listener from the shared UDP ICE
socket and lets the host advertise its provisioned public IP and UDP port.
Gateway TLS certificates remain configurable and their private keys never enter
worker or hosted-server environments. The source-level root cause is described
in the [upstream Pumpkin issue](https://github.com/Pumpkin-MC/Pumpkin/issues/3738).

`tools/verify_worker_runtime.py` exercises a trusted synthetic CA, certificate
name rejection, waiting and assigned-server status routing, malformed signaling,
assignment failure, and UDP RakNet/STUN/DTLS relay behavior. A physical Bedrock
client was intentionally not exercised at the user's direction; the automated
checks do not claim a real-client login.

Build with Rust 1.98.1, Git, CMake, and a C/C++ compiler:

```sh
tools/build_pumpkin_native.sh debug
```

The build script manages ignored output/native-pumpkin/source, applies host-control.patch, copies host_control.rs, builds the Kurbis control plugin for `wasm32-wasip2`, and writes Pumpkin under output/native-pumpkin/target/debug/pumpkin. Docker image builds also package the plugin and install it into each server's mounted plugin directory. Keep the included GPL-3.0 license with distributed Pumpkin source or binaries.
