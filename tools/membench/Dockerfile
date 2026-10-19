# Everything else is bind-mounted from the host; libatomic1/libstdc++6 are for the host Node binary.
FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates procps coreutils bash libatomic1 libstdc++6 \
 && rm -rf /var/lib/apt/lists/*
