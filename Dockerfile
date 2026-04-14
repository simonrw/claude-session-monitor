FROM rust:1-bookworm AS builder

WORKDIR /src
COPY . .
RUN cargo build --release -p csm-server

FROM gcr.io/distroless/cc-debian12

COPY --from=builder /src/target/release/csm-server /usr/local/bin/csm-server

EXPOSE 7685

ENTRYPOINT ["csm-server"]
