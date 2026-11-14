# wasm-bindgen-cli at the version kd240/web/app/Cargo.toml pins, built the
# way nixpkgs builds its own wasm-bindgen-cli_0_2_* (the CLI and the crate
# must be the same version). The flake's locked nixpkgs stops at 0.2.127.
{ buildWasmBindgenCli, fetchCrate, rustPlatform }:

buildWasmBindgenCli rec {
  src = fetchCrate {
    pname = "wasm-bindgen-cli";
    version = "0.2.129";
    hash = "sha256-pcecKQd7E8Opw6bkFoE569epUi7gh5qpQF1e5PJY6V8=";
  };

  cargoDeps = rustPlatform.fetchCargoVendor {
    inherit src;
    inherit (src) pname version;
    hash = "sha256-vmUrWVU7kPJJxO5qIVeAkwQyWDELO1Z4Z5gitz2kco8=";
  };
}
