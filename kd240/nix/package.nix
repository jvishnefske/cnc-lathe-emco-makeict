# emco-kd240, the service binary (kd240/README.md). Built from the
# repository: its path dependencies are sender/core and firmware/bridge-core,
# and its capture test decodes with tools/emco-capture.py.
{ lib, rustPlatform, python3 }:

let
  root = ../..;
  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ../tests
      ../ui
      ../../sender/core/Cargo.toml
      ../../sender/core/src
      ../../firmware/bridge-core/Cargo.toml
      ../../firmware/bridge-core/src
      ../../tools/emco-capture.py
    ];
  };
in
rustPlatform.buildRustPackage {
  pname = "emco-kd240";
  version = "0.1.0";
  inherit src;
  cargoRoot = "kd240";
  buildAndTestSubdir = "kd240";
  cargoLock.lockFile = ../Cargo.lock;

  # The capture test runs the repository's decoder over a pty capture.
  nativeCheckInputs = [ python3 ];

  meta = {
    description = "EMCO PC Turn 50 sender service for the Kria KD240 (virtual machine + listen-only RS-485 capture)";
    mainProgram = "emco-kd240";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
  };
}
