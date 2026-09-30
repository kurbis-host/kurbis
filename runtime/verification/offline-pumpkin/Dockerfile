# Protocol integration fixture only. Production runtime authentication stays enabled.
FROM melon-pumpkin:host-control
COPY runtime/verification/offline-pumpkin/entrypoint.sh /usr/local/bin/offline-fixture
ENTRYPOINT ["/bin/sh", "/usr/local/bin/offline-fixture"]
