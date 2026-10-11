# Sun Brewed Homestead core: one binary serving the UI, API and dispatcher,
# plus the system specs so the same image can run the provisioning commands.
FROM rust:1-slim-trixie AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p homestead-server --bin homestead

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /app homestead
WORKDIR /app
COPY --from=build /src/target/release/homestead /usr/local/bin/homestead
COPY homestead.toml ./homestead.toml
COPY discord ./discord
COPY documenso ./documenso
COPY legal ./legal
COPY linear ./linear
COPY zeffy ./zeffy
COPY buffer ./buffer
COPY github ./github
COPY brand ./brand
USER homestead
ENV BIND_ADDR=0.0.0.0:8080
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s CMD ["/bin/sh", "-c", "exec 3<>/dev/tcp/127.0.0.1/8080"]
CMD ["homestead", "serve"]
