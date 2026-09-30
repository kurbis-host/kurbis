# Real protocol verification clients

This is a disposable integration client, not a production gateway dependency.
Install with `npm ci --ignore-scripts` and run `node verify.mjs --edition java|bedrock --port PORT --address HOSTNAME --output RESULT.json`.

Java uses protocol 775 (26.1), supported by the pinned Pumpkin runtime reporting 26.2. Bedrock uses 1.26.40/protocol 2168 over the JavaScript RakNet backend. The tests complete actual login/configuration/spawn flows and decode native transfer packets; `--transfer-port` additionally requires the expected transfer and a second backend login. Offline identities avoid external Microsoft credentials only inside the disposable derived runtime image. Production authentication stays enabled.

`--command` and `--expect-item NAME:COUNT` can be repeated for Java gameplay/persistence assertions. The client acknowledges teleports and observes inventory packets. Every run writes JSON events and a success/failure result; timeout and unexpected disconnect/transfer fail verification.

The lockfile contains upstream native-build and authentication dependencies. The current npm audit reports nine findings: critical/high findings are in tar/axios/cmake-js paths used by native addon downloads/builds, which are disabled by `--ignore-scripts`; the selected direct Bedrock client uses only JS RakNet and does not import the convenience module that initializes a native backend. Moderate findings involve authentication and UUID buffer-writing paths; these tests use offline authentication and never supply caller-controlled UUID output buffers. This is an explicit reachability assessment, not a clean-audit claim. Do not enable package scripts or use this fixture for untrusted archive/authentication operations without addressing the affected dependencies.
