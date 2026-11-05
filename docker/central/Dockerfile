FROM rustlang/rust:nightly-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libzmq3-dev cmake clang && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release --bin api --no-default-features

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libzmq5 && rm -rf /var/lib/apt/lists/* && useradd --system --uid 10001 --create-home fcore && mkdir -p /var/lib/fcore/api /var/log/fcore/api && chown -R fcore:fcore /var/lib/fcore /var/log/fcore
COPY --from=build /src/target/release/api /usr/local/bin/api
USER fcore
WORKDIR /var/lib/fcore/api
ENTRYPOINT ["/usr/local/bin/api", "/etc/fcore/api/config.toml"]
