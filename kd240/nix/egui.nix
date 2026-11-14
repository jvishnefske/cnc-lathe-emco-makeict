# The egui screen (kd240/web, served by emco-kd240 at /egui/), built for
# wasm32 and run through wasm-bindgen: a directory with index.html,
# emco_egui.js and emco_egui_bg.wasm, which package.nix embeds when given it
# (`egui = ...`). The same steps as kd240/web/build.sh.
#
# Needs a Rust with the wasm32-unknown-unknown target (the flake's
# rust-overlay toolchain has it) and the wasm-bindgen-cli whose version
# kd240/web/app/Cargo.toml pins. That is 0.2.129, which nixos-unstable has
# and nixos-25.11 (the board's system) does not, so the flake builds this
# with its unstable nixpkgs. The output is the same on every platform.
{ lib, makeRustPlatform, rust, wasm-bindgen-cli, binaryen }:

let
  rustPlatform = makeRustPlatform { cargo = rust; rustc = rust; };
  src = lib.fileset.toSource {
    root = ../..;
    fileset = lib.fileset.unions [
      ../web/Cargo.toml
      ../web/Cargo.lock
      ../web/index.html
      ../web/rules/Cargo.toml
      ../web/rules/src
      ../web/rules/examples
      ../web/app/Cargo.toml
      ../web/app/src
      ../web/app/examples
      ../../sender/core/Cargo.toml
      ../../sender/core/src
    ];
  };
  pinned = builtins.head (builtins.match ''.*wasm-bindgen = "=([0-9.]+)".*''
    (builtins.readFile ../web/app/Cargo.toml));
in
assert lib.assertMsg (wasm-bindgen-cli.version == pinned)
  "kd240/nix/egui.nix: wasm-bindgen-cli is ${wasm-bindgen-cli.version}, kd240/web/app/Cargo.toml pins ${pinned}";
rustPlatform.buildRustPackage {
  pname = "emco-kd240-egui";
  version = "0.1.0";
  inherit src;
  cargoRoot = "kd240/web";
  cargoLock.lockFile = ../web/Cargo.lock;
  nativeBuildInputs = [ wasm-bindgen-cli binaryen ];

  buildPhase = ''
    runHook preBuild
    cd kd240/web
    cargo build --release --frozen --target wasm32-unknown-unknown -p emco-egui
    runHook postBuild
  '';
  # The rules and the screen logic are tested natively (`cargo test` in
  # kd240/web); this derivation only builds the browser bundle.
  doCheck = false;
  installPhase = ''
    runHook preInstall
    mkdir -p $out
    wasm-bindgen --target web --no-typescript --out-dir $out --out-name emco_egui \
      target/wasm32-unknown-unknown/release/emco_egui.wasm
    wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
      --enable-mutable-globals --enable-reference-types --enable-multivalue \
      -o $out/emco_egui_bg.opt.wasm $out/emco_egui_bg.wasm
    mv $out/emco_egui_bg.opt.wasm $out/emco_egui_bg.wasm
    cp index.html $out/
    runHook postInstall
  '';

  meta = {
    description = "The emco_lathe operator screen for emco-kd240, egui on wasm32";
    license = lib.licenses.mit;
    platforms = lib.platforms.all;
  };
}
