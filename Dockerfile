FROM rust:1.97-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY crates ./crates
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --create-home miser
COPY --from=builder /app/target/release/miser-gateway /usr/local/bin/miser-gateway
COPY config /etc/miser
USER miser
EXPOSE 8787
ENTRYPOINT ["miser-gateway"]
CMD ["--config", "/etc/miser/miser.toml"]
