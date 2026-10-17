# Helm hub: a static musl binary on Alpine. Build context is hub/.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked && cp target/release/helm-hub /helm-hub

FROM alpine:3
RUN mkdir -p /data && chown 1000:100 /data
COPY --from=build /helm-hub /usr/local/bin/helm-hub
USER 1000:100
ENV HUB_DB=/data/hub.db HUB_BIND=0.0.0.0:8000
VOLUME /data
EXPOSE 8000
HEALTHCHECK --interval=60s --timeout=5s --retries=3 \
  CMD wget -qO- http://127.0.0.1:8000/health >/dev/null || exit 1
ENTRYPOINT ["helm-hub"]
CMD ["serve"]
