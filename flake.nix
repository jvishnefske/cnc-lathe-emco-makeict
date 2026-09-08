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

        # Three targets, for three things in the tree:
        #   host      sender/core and firmware/bridge-core, both `cargo test`ed
        #             natively with no browser and no board
        #   wasm32    the browser sender UI
        #   thumbv7em firmware/rs485-bridge, the USB<->RS-485 capture bridge
        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
          targets = [ "wasm32-unknown-unknown" "thumbv7em-none-eabihf" ];
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
            socat              # pty pairs for testing tools/virtual-emco.py
            picocom            # manual serial poking during protocol discovery

            # Flashing and debugging firmware/rs485-bridge over the board's
            # ST-LINK. Not needed to BUILD it, and not needed at all for the
            # host-side tests, which is most of what there is to run.
            probe-rs-tools

            git
          ];

          shellHook = ''
            # The pre-commit hook lives in .githooks/ so it is version
            # controlled; .git/hooks is not. Pointing git at it is one command,
            # and one command nobody runs is a hook nobody has. Set only if it
            # is unset, so a deliberate choice of somewhere else survives, and
            # undone with `git config --unset core.hooksPath`.
            if [ -d .githooks ] && [ -z "$(git config --get core.hooksPath 2>/dev/null)" ]; then
              git config core.hooksPath .githooks
              echo "  hooks       : pre-commit rustfmt + clippy enabled (.githooks)"
            fi
            echo "EMCO PC Turn 50 dev shell"
            echo "  sender/core : cargo test        (native, no browser)"
            echo "  sender/web  : npm ci && npm run dev"
            echo "  sim/        : docker compose -f sim/compose.yml up"
            echo "  tools/      : python3 tools/virtual-emco.py"
            echo "  firmware/   : ./firmware/check.sh   (no board needed)"
          '';
        };
      });
}
