# The fleet server's image (kd240/fleet/README.md). Build from kd240/fleet:
#
#   docker build -t emco-fleet-server .
#
# Or build it with Nix instead: nix/image.nix.
FROM docker.io/library/rust:1.91-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY proto proto
COPY server server
COPY agent agent
COPY cli cli
RUN cargo build --release --locked -p emco-fleet-server \
 && mkdir -p /out/data /out/cache

# glibc, no shell, uid 65532.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/emco-fleet-server /usr/local/bin/emco-fleet-server
# A named volume on /data starts with this ownership.
COPY --from=build --chown=65532:65532 /out/data /data
COPY --from=build /out/cache /cache
ENV FLEET_LISTEN=0.0.0.0:8080 FLEET_DATA_DIR=/data FLEET_CACHE_DIR=/cache
EXPOSE 8080
VOLUME ["/data", "/cache"]
ENTRYPOINT ["/usr/local/bin/emco-fleet-server"]
