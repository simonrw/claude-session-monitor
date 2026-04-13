FROM rust:1-bookworm AS builder

RUN rustup default beta

WORKDIR /src
COPY . .
RUN cargo build --release -p server

FROM gcr.io/distroless/cc-debian12

COPY --from=builder /src/target/release/server /usr/local/bin/server

EXPOSE 7685

ENTRYPOINT ["server"]
