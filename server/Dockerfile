FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations migrations
COPY src src
RUN cargo build --release --locked

FROM debian:bookworm-slim
COPY --from=build /src/target/release/notesync-server /usr/local/bin/notesync-server
ENV NOTESYNC_BIND_ADDR=0.0.0.0:8080 \
    NOTESYNC_DATA_DIR=/data
VOLUME /data
EXPOSE 8080
# Matches the default "apps" user on TrueNAS; override with `user:` in compose.
USER 568:568
ENTRYPOINT ["notesync-server"]
CMD ["serve"]
