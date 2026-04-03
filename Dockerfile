# ─── Builder stage ───
FROM rust:1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo build --release --bin ostia

# ─── Runtime stage ───
FROM debian:bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
    git \
    curl \
    wget \
    jq \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/ostia /usr/local/bin/ostia
COPY docker/config.yaml /etc/ostia/config.yaml

RUN useradd -r -m -s /bin/bash ostia
USER ostia
WORKDIR /workspace

EXPOSE 8080

ENTRYPOINT ["ostia"]
CMD ["serve", "--config", "/etc/ostia/config.yaml", "--transport", "http", "--host", "0.0.0.0", "--port", "8080"]
