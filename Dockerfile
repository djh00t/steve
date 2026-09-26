FROM rust:1.90-bookworm AS build
WORKDIR /src
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN useradd --system --create-home --uid 10001 steve
COPY --from=build /src/target/release/steve /usr/local/bin/steve
USER steve
WORKDIR /home/steve
EXPOSE 11435
ENTRYPOINT ["steve"]
CMD ["serve"]
