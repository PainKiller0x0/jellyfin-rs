FROM docker.m.daocloud.io/library/node:24-bookworm-slim AS admin-builder
WORKDIR /app/admin
RUN npm install -g pnpm@11.15.1 --registry=https://registry.npmmirror.com
COPY admin/package.json admin/pnpm-lock.yaml admin/pnpm-workspace.yaml ./
RUN pnpm config set registry https://registry.npmmirror.com && pnpm install --frozen-lockfile
COPY admin/ ./
RUN pnpm build

FROM docker.m.daocloud.io/library/rust:1.97.1-bookworm AS builder
WORKDIR /app
ENV CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse
ENV CARGO_REGISTRIES_CRATES_IO_INDEX=sparse+https://rsproxy.cn/index/
ENV CARGO_HTTP_MULTIPLEXING=false
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo fetch
COPY src src
COPY data data
RUN cargo build --release

FROM docker.m.daocloud.io/library/debian:bookworm-slim
WORKDIR /
RUN sed -i 's|deb.debian.org|mirrors.aliyun.com|g' /etc/apt/sources.list.d/debian.sources && apt-get update && apt-get install -y --no-install-recommends ca-certificates ffmpeg && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/jellyfin-rs /usr/local/bin/jellyfin-rs
COPY --from=admin-builder /app/admin/dist /admin/dist
ENV JELLYFIN_RS_HOST=0.0.0.0
ENV JELLYFIN_RS_PORT=8096
EXPOSE 8096
CMD ["jellyfin-rs"]
