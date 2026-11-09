FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations migrations
COPY src src
RUN cargo build --release --locked

FROM debian:bookworm-slim
COPY --from=build /src/target/release/notesync-server /usr/local/bin/notesync-server
ENV NOTESYNC_BIND_ADDR=0.0.0.0:8080 \
    NOTESYNC_ADMIN_BIND_ADDR=0.0.0.0:8081 \
    NOTESYNC_DATA_DIR=/data
VOLUME /data
# 8080: sync API. 8081: admin page.
EXPOSE 8080 8081
# Matches the default "apps" user on TrueNAS; override with `user:` in compose.
USER 568:568
ENTRYPOINT ["notesync-server"]
CMD ["serve"]
