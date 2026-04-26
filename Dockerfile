FROM oven/bun:1-debian AS web

WORKDIR /web
COPY web/bun.lock web/package.json ./
RUN bun install --frozen-lockfile
COPY web/ .
RUN bun run build

FROM rust:1-bookworm AS builder

ARG SENTRY_DSN
ENV SENTRY_DSN=$SENTRY_DSN

WORKDIR /src
COPY . .
RUN cargo build --release -p csm-server

FROM gcr.io/distroless/cc-debian12

COPY --from=builder /src/target/release/csm-server /usr/local/bin/csm-server
COPY --from=web /web/dist /srv/web

EXPOSE 7685

ENTRYPOINT ["csm-server", "--static-dir", "/srv/web"]
