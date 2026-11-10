#!/usr/bin/env bash
# Builds the web viewer into crates/web/dist: study-web.js + study-web.wasm (Emscripten output)
# next to study-viewer.js (the API pages import). Needs the Emscripten SDK (5.0 or newer) in
# $EMSDK or ~/emsdk, and `rustup target add wasm32-unknown-emscripten`.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
emsdk="${EMSDK:-$HOME/emsdk}"
export EMSDK="$emsdk"
export EM_CONFIG="$emsdk/.emscripten"
export PATH="$emsdk/upstream/emscripten:$emsdk/upstream/bin:$PATH"
# The SDK's own Python and Node, which its tools run on (Windows has neither by default).
for tool in "$emsdk"/python/*/ "$emsdk"/node/*/bin/; do
    [ -d "$tool" ] && PATH="$tool:$PATH"
done
# Skia is C++, so link with em++ (rustc would default to emcc.bat, gone from newer SDKs).
export CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_LINKER=em++

profile="${1:-release}"
cargo build -p study-web --target wasm32-unknown-emscripten --profile "$profile"

out="$here/../../target/wasm32-unknown-emscripten/$profile"
dist="$here/dist"
mkdir -p "$dist"
cp "$out/study-web.js" "$out/study_web.wasm" "$here/js/study-viewer.js" "$here/js/study-sandbox.html" "$dist/"
# Demo pages: serve dist/ and open demo.html (?file=<url of a .pen>) or compare.html.
cp "$here/js/demo.html" "$here/js/compare.html" "$here/../../examples/showcase.pen" "$dist/"
ls -l "$dist"
