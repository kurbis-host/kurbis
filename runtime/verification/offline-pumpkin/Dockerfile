# Protocol integration fixture only. Production runtime authentication stays enabled.
ARG PUMPKIN_BASE_IMAGE=melon-pumpkin:host-control
FROM ${PUMPKIN_BASE_IMAGE}
COPY runtime/verification/offline-pumpkin/entrypoint.sh /usr/local/bin/offline-fixture
ENTRYPOINT ["/bin/sh", "/usr/local/bin/offline-fixture"]
