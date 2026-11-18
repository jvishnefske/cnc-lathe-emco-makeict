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
#
# `middlewareSrc`: the middleware checkout (the flake's `middleware` input).
# The workspace reaches it by path, `../../../middleware-posix-node` from
# kd240/fleet, so the source is laid out as `<root>/lathe/kd240/fleet`
# beside `<root>/middleware-posix-node`. (When middleware becomes a cargo git dependency this
# turns into cargoLock.outputHashes.)
{ lib, rustPlatform, runCommand, middlewareSrc, members ? [ ] }:

let
  fleet = lib.fileset.toSource {
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
  src = runCommand "emco-fleet-src" { } ''
    mkdir -p $out/lathe/kd240
    cp -r ${fleet} $out/lathe/kd240/fleet
    cp -r ${middlewareSrc} $out/middleware-posix-node
    chmod -R u+w $out
  '';
  pick = lib.concatMap (m: [ "-p" m ]) members;
in
rustPlatform.buildRustPackage {
  pname = if members == [ ] then "emco-fleet" else lib.concatStringsSep "+" members;
  version = "0.1.0";
  inherit src;
  sourceRoot = "emco-fleet-src/lathe/kd240/fleet";
  cargoLock.lockFile = ../Cargo.lock;
  cargoBuildFlags = pick;
  cargoTestFlags = pick;

  meta = {
    description = "KD240 fleet: telemetry and owner-signed A/B updates (server, board agent, build-host CLI)";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram =
      if members == [ "emco-fleet-agent" ] then "emco-fleet-agent"
      else if members == [ "emco-fleet-server" ] then "emco-fleet-server"
      else "emco-fleet";
  };
}
