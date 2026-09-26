FROM rust:1.90-bookworm AS build
WORKDIR /src
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --uid 10001 steve \
    && mkdir -p /data/objects \
    && chown -R steve:steve /data
COPY --from=build /src/target/release/steve /usr/local/bin/steve
USER steve
WORKDIR /home/steve
ENV STEVE_DATABASE_URL="sqlite:///data/steve.db?mode=rwc" \
    STEVE_OBJECT_STORE="fs" \
    STEVE_OBJECT_ROOT="/data/objects" \
    STEVE_INFERENCE_BIND="[::]:11435" \
    STEVE_MANAGEMENT_BIND="[::]:8790"
EXPOSE 11435 8790
ENTRYPOINT ["steve"]
CMD ["serve"]
