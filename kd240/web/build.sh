#!/usr/bin/env bash
# Builds the egui screen into kd240/web/dist/, which emco-kd240 embeds at
# /egui/ when it is there (kd240/build.rs). Run from anywhere:
#
#   kd240/web/build.sh            then: (cd kd240 && cargo build)
#
# Needs the wasm32-unknown-unknown Rust target and wasm-bindgen-cli of the
# exact version app/Cargo.toml pins (0.2.129). If `wasm-bindgen` on PATH is
# another version, it is taken from `nix shell nixpkgs#wasm-bindgen-cli`.
#
# wasm-opt (binaryen) shrinks the module by about a quarter. An old one
# breaks it: Ubuntu 24.04's binaryen 108 emits a module whose externref
# table cannot grow, so the page dies at start. So wasm-opt is used only at
# version 116 or later, from PATH or `nix shell nixpkgs#binaryen`, and is
# skipped otherwise. Whatever the result, it is instantiated under Node
# (check-wasm.mjs) when node is available, and the build fails if it cannot
# start.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
cd "$here"
want=$(sed -n 's/^wasm-bindgen = "=\(.*\)"/\1/p' app/Cargo.toml)

cargo build --release --target wasm32-unknown-unknown -p emco-egui
wasm=target/wasm32-unknown-unknown/release/emco_egui.wasm

bindgen=(wasm-bindgen)
have=$(wasm-bindgen --version 2>/dev/null | awk '{print $2}' || true)
if [ "$have" != "$want" ]; then
    command -v nix >/dev/null || { echo "build.sh: need wasm-bindgen $want (have '${have:-none}') and no nix to fetch it" >&2; exit 1; }
    bindgen=(nix shell nixpkgs#wasm-bindgen-cli -c wasm-bindgen)
    have=$("${bindgen[@]}" --version | awk '{print $2}')
    [ "$have" = "$want" ] || { echo "build.sh: nixpkgs has wasm-bindgen $have, the crate pins $want; change the pin in app/Cargo.toml" >&2; exit 1; }
fi

rm -rf dist.tmp && mkdir dist.tmp
"${bindgen[@]}" --target web --no-typescript --out-dir dist.tmp --out-name emco_egui "$wasm"
opt=()
v=$(wasm-opt --version 2>/dev/null | awk '{print $3}' || true)
if [ -n "$v" ] && [ "$v" -ge 116 ] 2>/dev/null; then
    opt=(wasm-opt)
elif command -v nix >/dev/null; then
    opt=(nix shell nixpkgs#binaryen -c wasm-opt)
fi
if [ ${#opt[@]} -gt 0 ]; then
    # The features rustc's wasm32-unknown-unknown target enables by default.
    "${opt[@]}" -Oz --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
        --enable-mutable-globals --enable-reference-types --enable-multivalue \
        -o dist.tmp/emco_egui_bg.opt.wasm dist.tmp/emco_egui_bg.wasm
    mv dist.tmp/emco_egui_bg.opt.wasm dist.tmp/emco_egui_bg.wasm
else
    echo "build.sh: no usable wasm-opt (need binaryen >= 116); the bundle is unoptimised (larger) but works" >&2
fi
cp index.html dist.tmp/
if command -v node >/dev/null; then
    printf '{"type":"module"}\n' > dist.tmp/package.json   # for the check only
    node check-wasm.mjs dist.tmp
    rm dist.tmp/package.json
else
    echo "build.sh: no node, so the bundle was not test-instantiated" >&2
fi
rm -rf dist && mv dist.tmp dist
ls -l dist
