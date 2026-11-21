# emco-mesh-enroll (kd240/scripts/emco-mesh-enroll): the board's key, CSR
# and certificate for the uplink. The same derivation runs on the board
# (aarch64 pkgs, mesh.nix) and in checks.x86_64-linux.kd240-enroll.
{ lib
, writeShellApplication
, openssl
, coreutils
, gnugrep
, gnused
, diffutils
, getent
, iproute2
, tlsDir ? "/var/lib/emco-mesh/tls"
}:

writeShellApplication {
  name = "emco-mesh-enroll";
  # systemctl comes from the system's PATH (the running systemd's own).
  runtimeInputs = [ openssl coreutils gnugrep gnused diffutils getent iproute2 ];
  runtimeEnv.EMCO_ENROLL_DEFAULT_DIR = tlsDir;
  text = builtins.readFile ../scripts/emco-mesh-enroll;
  meta.description = "The KD240's certificate for its uplink to the cloud mesh";
  meta.mainProgram = "emco-mesh-enroll";
}
