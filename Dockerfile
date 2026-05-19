FROM rust:1.85 AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo fetch
COPY src src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/jellyfin-rs /usr/local/bin/jellyfin-rs
ENV JELLYFIN_RS_HOST=0.0.0.0
ENV JELLYFIN_RS_PORT=8096
EXPOSE 8096
CMD ["jellyfin-rs"]
