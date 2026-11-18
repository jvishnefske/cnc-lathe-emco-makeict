# The fleet server's image (kd240/fleet/README.md).
#
# Build from kd240/fleet:
#
#   docker build -t emco-fleet-server .
#
# Or with Nix: `nix build .#emco-fleet-image` (nix/image.nix).
FROM docker.io/library/rust:1.91-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY proto proto
COPY server server
COPY agent agent
COPY cli cli
RUN cargo build --release --locked -p emco-fleet-server \
 && mkdir -p /out/data

# glibc, no shell, uid 65532.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/emco-fleet-server /usr/local/bin/emco-fleet-server
# A named volume on /data starts with this ownership (state, and the
# bundle files in /data/files).
COPY --from=build --chown=65532:65532 /out/data /data
ENV FLEET_LISTEN=0.0.0.0:8080 FLEET_DATA_DIR=/data
EXPOSE 8080
VOLUME ["/data"]
ENTRYPOINT ["/usr/local/bin/emco-fleet-server"]
