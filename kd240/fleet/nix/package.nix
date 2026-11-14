# The fleet workspace (kd240/fleet): emco-fleet-server, emco-fleet-agent and
# the build host's emco-fleet CLI. `members` picks which crates to build, so
# the board's agent package does not carry the server:
#
#   pkgs.callPackage ./package.nix { }                                   # everything
#   pkgs.callPackage ./package.nix { members = [ "emco-fleet-agent" ]; } # the board
#   pkgs.callPackage ./package.nix { members = [ "emco-fleet-server" ]; }
#
# The checks run the workspace's tests, the end-to-end one included (it
# serves on 127.0.0.1, which the build sandbox has).
{ lib, rustPlatform, members ? [ ] }:

let
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../proto
      ../server
      ../agent
      ../cli
    ];
  };
  pick = lib.concatMap (m: [ "-p" m ]) members;
in
rustPlatform.buildRustPackage {
  pname = if members == [ ] then "emco-fleet" else lib.concatStringsSep "+" members;
  version = "0.1.0";
  inherit src;
  cargoLock.lockFile = ../Cargo.lock;
  cargoBuildFlags = pick;
  cargoTestFlags = pick;

  meta = {
    description = "KD240 fleet: telemetry and owner-signed NixOS updates (server, board agent, build-host CLI)";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram =
      if members == [ "emco-fleet-agent" ] then "emco-fleet-agent"
      else if members == [ "emco-fleet-server" ] then "emco-fleet-server"
      else "emco-fleet";
  };
}
