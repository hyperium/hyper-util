check-client-features:
    cargo hack --no-dev-deps check --feature-powerset --depth 2 \
      --features=client \
      --include-features client-legacy,client-legacy-http-connector,client-pool,client-proxy,client-proxy-system,http1,http2,tokio,tracing

check-server-features:
    cargo hack --no-dev-deps check --feature-powerset --depth 2 \
      --features=server \
      --include-features server-auto,server-graceful,http1,http2,tokio,tracing
