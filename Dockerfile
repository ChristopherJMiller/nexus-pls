FROM docker.io/rust:1.62-alpine as BUILDER

WORKDIR /app

RUN apk add musl-dev libc-dev

ADD . .

RUN --mount=type=cache,target=/app/target cargo build --release && cp /app/target/release/nexus-pls /app/nexus-pls

FROM gcr.io/distroless/cc

WORKDIR /app

COPY --from=BUILDER /app/nexus-pls .

CMD ["/app/nexus-pls"]
