FROM node:24-bookworm-slim AS admin-builder
WORKDIR /app/admin
RUN npm install -g pnpm@11.15.1
COPY admin/package.json admin/pnpm-lock.yaml admin/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY admin/ ./
RUN pnpm build

FROM rust:1.97.1 AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo fetch
COPY src src
COPY data data
RUN cargo build --release

FROM debian:bookworm-slim
WORKDIR /
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates ffmpeg && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/jellyfin-rs /usr/local/bin/jellyfin-rs
COPY --from=admin-builder /app/admin/dist /admin/dist
ENV JELLYFIN_RS_HOST=0.0.0.0
ENV JELLYFIN_RS_PORT=8096
EXPOSE 8096
CMD ["jellyfin-rs"]
