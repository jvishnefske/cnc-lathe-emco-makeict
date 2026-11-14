{
  description = "EMCO PC Turn 50 — docs, browser G-code sender, LinuxCNC simulator";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # LinuxCNC is built from a stable release: PyQt5 5.15, which QtVCP and
    # the emco_lathe screen need, does not build on current unstable (its SIP
    # targets an ABI PyQt5 5.15 does not support). 25.11 has it prebuilt.
    nixpkgs-linuxcnc.url = "github:NixOS/nixpkgs/nixos-25.11";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, nixpkgs-linuxcnc, flake-utils, rust-overlay }:
    {
      # The Kria KD240's SD card and its over-the-network updates
      # (kd240/nix/configuration.nix). On the stable release, like LinuxCNC:
      #   nix build .#nixosConfigurations.kd240.config.system.build.sdImage
      #   nixos-rebuild switch --flake .#kd240 --target-host admin@kd240.local --sudo
      nixosConfigurations.kd240 = nixpkgs-linuxcnc.lib.nixosSystem {
        modules = [ ./kd240/nix/configuration.nix ];
        # The egui screen, built on unstable for the deploy host (it needs a
        # wasm-bindgen-cli 25.11 lacks). wasm32 is the same on every
        # platform, so the board's aarch64 service embeds this build. The
        # host is x86-64 Linux already, for the cross-compiled kernel.
        specialArgs.kd240Egui = self.packages.x86_64-linux.kd240-egui;
      };
    } //
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

        # LinuxCNC is not in nixpkgs, so the simulator and its emco_lathe
        # screen run in the container (sim/). What runs here is everything
        # that does not need LinuxCNC: the screen's rules, which are plain
        # Python on purpose, checked against the real example programs.
        rulesSrc = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./linuxcnc/sim/emco-pcturn50/qtvcp/screens/emco_lathe/emco_rules.py
            ./linuxcnc/sim/emco-pcturn50/tests
            ./gcode
          ];
        };

        # The container wrappers. They need Docker on the host, which Nix
        # cannot provide, so they say so instead of failing obscurely.
        dockerApp = name: text: {
          type = "app";
          program = pkgs.lib.getExe (pkgs.writeShellApplication {
            inherit name;
            text = ''
              command -v docker >/dev/null || { echo "${name}: needs Docker (LinuxCNC is not in nixpkgs)" >&2; exit 1; }
              cd "$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
              [ -f sim/compose.yml ] || { echo "${name}: run it from inside the repository" >&2; exit 1; }
            '' + text;
          });
        };

        # LinuxCNC itself, built from source (nix/linuxcnc.nix), so the
        # simulator can run with no Docker at all.
        lcncPkgs = nixpkgs-linuxcnc.legacyPackages.${system};
        yapps2 = lcncPkgs.python3.pkgs.callPackage ./nix/yapps2.nix { };
        linuxcnc = lcncPkgs.callPackage ./nix/linuxcnc.nix { inherit yapps2; };
        # The egui screen for emco-kd240 (kd240/web), and the service with
        # it embedded, for this machine. Built on unstable: the screen needs
        # wasm-bindgen-cli 0.2.129 (kd240/nix/wasm-bindgen-cli.nix), which
        # nixos-25.11, the board's system, does not have. The board's
        # nixosConfiguration still builds the service without the screen.
        kd240-egui = pkgs.callPackage ./kd240/nix/egui.nix {
          inherit rust;
          wasm-bindgen-cli = pkgs.callPackage ./kd240/nix/wasm-bindgen-cli.nix { };
        };
        emco-kd240 = pkgs.callPackage ./kd240/nix/package.nix { egui = kd240-egui; };
      in
      {
        packages = {
          inherit linuxcnc yapps2 kd240-egui emco-kd240;
          default = linuxcnc;
        };

        checks.emco-rules = pkgs.runCommand "emco-rules-tests" { nativeBuildInputs = [ pkgs.python3 ]; } ''
          cd ${rulesSrc}
          # The rules write nothing outside a temp dir, but keep HOME honest.
          HOME=$TMPDIR python3 -B linuxcnc/sim/emco-pcturn50/tests/test_emco_rules.py
          touch $out
        '';

        apps.sim = dockerApp "emco-sim" ''
          echo "  open http://localhost:8080/vnc.html?autoconnect=1&resize=scale"
          exec docker compose -f sim/compose.yml up "$@"
        '';

        # Both container tests expect an un-referenced machine, so start fresh.
        apps.sim-test = dockerApp "emco-sim-test" ''
          docker compose -f sim/compose.yml down
          docker compose -f sim/compose.yml up -d
          trap 'docker compose -f sim/compose.yml down' EXIT
          # `up -d` returns before LinuxCNC has started; wait until it answers.
          for _ in $(seq 90); do
            docker exec emco-pcturn50-sim python3 -c 'import linuxcnc; linuxcnc.stat().poll()' 2>/dev/null && break
            sleep 1
          done
          docker exec emco-pcturn50-sim python3 /repo/sim/acceptance-test.py
          docker exec emco-pcturn50-sim python3 /repo/sim/frontend-smoke.py
        '';

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
            echo "  sim/        : nix run .#sim        (Docker; LinuxCNC is not in nixpkgs)"
            echo "                nix run .#sim-test   (acceptance + emco_lathe smoke test)"
            echo "  screen rules: nix flake check      (no LinuxCNC needed)"
            echo "  tools/      : python3 tools/virtual-emco.py"
            echo "  firmware/   : ./firmware/check.sh   (no board needed)"
          '';
        };
      });
}
