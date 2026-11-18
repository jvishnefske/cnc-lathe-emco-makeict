# The fleet server's image (kd240/fleet/README.md).
#
# The workspace depends on middleware by path (../../../middleware-posix-node) while
# that branch is unpublished, so the build context is the directory that
# holds both checkouts:
#
#   cd ../../..   # the parent of cnc-lathe-emco-makeict and middleware
#   docker build -f cnc-lathe-emco-makeict/kd240/fleet/Dockerfile \
#     --build-arg LATHE=cnc-lathe-emco-makeict -t emco-fleet-server .
#
# Or, simpler, with Nix: `nix build .#emco-fleet-image` (nix/image.nix).
FROM docker.io/library/rust:1.91-slim-bookworm AS build
ARG LATHE=cnc-lathe-emco-makeict
WORKDIR /src
# (middleware/target is large: a .dockerignore beside both checkouts with
# `*/target` keeps it out of the context.)
COPY middleware-posix-node middleware-posix-node
COPY ${LATHE}/kd240/fleet lathe/kd240/fleet
WORKDIR /src/lathe/kd240/fleet
RUN rm -rf target \
 && cargo build --release --locked -p emco-fleet-server \
 && mkdir -p /out/data

# glibc, no shell, uid 65532.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/lathe/kd240/fleet/target/release/emco-fleet-server /usr/local/bin/emco-fleet-server
# A named volume on /data starts with this ownership (state, and the
# bundle files in /data/files).
COPY --from=build --chown=65532:65532 /out/data /data
ENV FLEET_LISTEN=0.0.0.0:8080 FLEET_DATA_DIR=/data
EXPOSE 8080
VOLUME ["/data"]
ENTRYPOINT ["/usr/local/bin/emco-fleet-server"]
