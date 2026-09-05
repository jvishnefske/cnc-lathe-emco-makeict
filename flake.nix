{
  description = "EMCO PC Turn 50 — docs, browser G-code sender, LinuxCNC simulator";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # The sender core compiles both natively (for `cargo test`, no browser
        # needed) and to wasm32 (for the browser UI), so the toolchain carries
        # both targets.
        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
          targets = [ "wasm32-unknown-unknown" ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rust
            wasm-pack
            wasm-bindgen-cli
            binaryen           # wasm-opt

            nodejs_22           # npm ships with it

            (python3.withPackages (ps: with ps; [ pyserial ]))

            imagemagick        # photo curation for docs/img
            socat              # pty pairs for testing tools/virtual-ac95.py
            picocom            # manual serial poking during protocol discovery

            git
          ];

          shellHook = ''
            echo "EMCO PC Turn 50 dev shell"
            echo "  sender/core : cargo test        (native, no browser)"
            echo "  sender/web  : npm ci && npm run dev"
            echo "  sim/        : docker compose -f sim/compose.yml up"
            echo "  tools/      : python3 tools/virtual-ac95.py"
          '';
        };
      });
}
