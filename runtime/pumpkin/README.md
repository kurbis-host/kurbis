# Hosted Pumpkin runtime

The hosted runtime builds Pumpkin directly on the local host with
`tools/build_pumpkin_native.sh`; local development does not use Docker. The
source pin is Pumpkin `0.2.0+26.3-26.51`, which speaks Java protocol 777 and
Bedrock 1.26.51/protocol 2193.

The checked-in integration adds the authenticated Unix host-control socket
used by workers for player state, messages, transfers, saves, whitelist and
operator updates, and shutdown. Snapshot saves flush watched chunk regions and
return write failures so the worker can avoid marking an incomplete recovery
point as complete. `tools/protocol/verify_java_263.py` exercises real protocol
777 logins, transfers, commands, inventory, and reconnect persistence.

Bedrock 1.26.51 uses NetherNet. The pinned upstream TCP signaling endpoint
serves plain HTTP while current Bedrock clients initiate TLS, so Bedrock login
requires a TLS termination path that this local runtime does not provide yet.
See the [upstream Pumpkin issue](https://github.com/Pumpkin-MC/Pumpkin/issues/3738).
Java 26.3 login and hosting behavior are verified independently.

Build with Rust 1.98.1, Git, CMake, and a C/C++ compiler:

```sh
tools/build_pumpkin_native.sh debug
```

The build script manages only ignored output/native-pumpkin/source, applies host-control.patch, copies host_control.rs, and writes the executable under output/native-pumpkin/target/debug/pumpkin. Keep the included GPL-3.0 license with distributed Pumpkin source or binaries.
