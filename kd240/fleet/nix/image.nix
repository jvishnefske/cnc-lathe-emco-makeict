# The server as an OCI image built by Nix, the alternative to ../Dockerfile:
#
#   nix build --impure --expr \
#     '(import <nixpkgs> {}).callPackage ./kd240/fleet/nix/image.nix {}'
#   docker load < result        # emco-fleet-server:latest
#
# It holds the server binary and nothing else (no shell). It runs as
# 65532:65532, like the Dockerfile's distroless image, so the compose file's
# volumes work with either.
{ lib, dockerTools, callPackage, server ? callPackage ./package.nix { members = [ "emco-fleet-server" ]; } }:

dockerTools.buildLayeredImage {
  name = "emco-fleet-server";
  tag = "latest";
  contents = [ server ];
  # A named volume on /data starts with this ownership.
  fakeRootCommands = ''
    mkdir -p data cache
    chown 65532:65532 data
  '';
  config = {
    Entrypoint = [ (lib.getExe' server "emco-fleet-server") ];
    User = "65532:65532";
    ExposedPorts."8080/tcp" = { };
    Env = [
      "FLEET_LISTEN=0.0.0.0:8080"
      "FLEET_DATA_DIR=/data"
      "FLEET_CACHE_DIR=/cache"
    ];
    Volumes = {
      "/data" = { };
      "/cache" = { };
    };
  };
}
