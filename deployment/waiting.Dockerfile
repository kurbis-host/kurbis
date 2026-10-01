FROM melon-pumpkin:host-control

USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends netcat-openbsd \
    && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /run/kurbis-gateway \
    && chown 2613:2613 /run/kurbis-gateway \
    && chmod 0770 /run/kurbis-gateway
COPY deployment/waiting.toml /pumpkin/pumpkin.toml
HEALTHCHECK --interval=5s --timeout=2s --start-period=30s --retries=12 \
    CMD printf '{"token":"%s","action":"status"}\n' "$PUMPKIN_HOST_CONTROL_TOKEN" | nc -U -w 1 /run/kurbis-gateway/host-control.sock | grep -q '"ok":true' || exit 1
USER 2613:2613
ENTRYPOINT ["/usr/local/bin/pumpkin"]
