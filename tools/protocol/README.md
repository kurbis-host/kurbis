# Java 26.3 protocol verifier

`verify_java_263.py` is a standard-library client for Java protocol 777. It
performs the real login, configuration and play handshakes used by local
runtime verification, and can check waiting joins, transfers, commands,
inventory, server messages and disconnects. It writes a JSON evidence file for
each run.

The host-control integration check also sends protocol 776 (Java 26.2) and
confirms the server rejects it with the expected outdated-client reason.

Run it directly with Python 3, or through the Node wrapper used by lifecycle
tools:

```sh
python3 verify_java_263.py --edition java --port PORT --address HOSTNAME --output RESULT.json
node verify.mjs --edition java --port PORT --address HOSTNAME --output RESULT.json
```

The client uses no npm dependencies. Bedrock 1.26.51 uses NetherNet; the pinned
Pumpkin signaling endpoint currently returns plain HTTP to clients that begin
with TLS, so Bedrock login is not covered by this verifier. See
[Pumpkin issue #3738](https://github.com/Pumpkin-MC/Pumpkin/issues/3738).
