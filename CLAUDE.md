# study

Local Rust replacement for the Pen (pen.dev) desktop app: renders `.pen` design files on a Skia
canvas and serves Pen's MCP tools (`get_app_state`, `execute`, `read_skill`, `get_style`) so Claude
designs while the user watches. No manual editing UI by design. Runs on macOS (the user's main
machine; Apple Silicon) and Windows. Platform code is `#[cfg]`-gated in `gui.rs` (menus, icons, GL
profile) and `mcp.rs` (process detaching).
User-facing docs: [README.md](README.md). Implementation notes: [docs/implementation.md](docs/implementation.md) —
read the relevant section before changing file I/O, layout, rendering, the execute API or IPC.

## Layout

- `crates/pen` — document model with lossless JSON round-trip, variables/themes, component
  instances, `imports` of other files, Pen's flex layout, validation. No graphics deps; layout
  tests in `tests/layout.rs`.
- `crates/render` — Skia renderer: fonts (Google Fonts download), text (SkParagraph), icons (npm
  tarballs), images (incl. WebP via image-webp), GLSL→SkSL shaders, mesh gradients,
  culling/LOD/thumbnails, offscreen rendering.
- `crates/study` — the binary: window + menus (`gui.rs`), MCP stdio server (`mcp.rs`), IPC between
  MCP process and windows (`ipc.rs`), shared document state (`engine.rs`), QuickJS execute API
  (`api.rs` + `prelude.js`), `script` node runner (`scripts.rs`), SVG import for `Generate` (`svg.rs`),
  screenshots/export/HTML.
- `crates/study/docs/*.md` — the docs *Claude-as-designer* reads via `read_skill`/`get_app_state`
  (compiled in with `include_str!`; rebuild after editing). Keep them accurate to the API.
- `assets/` — `logo.svg` and the icon rendered from it (`cargo run -p render --example mkicon -- assets/logo.svg assets`).

## Commands

```bash
cargo build --release                       # target/release/study.exe
cargo test --workspace                      # layout, undo, SVG import, script tests
cargo run -p pen --example roundtrip -- <files.pen...>   # each must re-serialize byte-identical
cargo install --path crates/study           # what the user's MCP registration runs
```

Manual end-to-end checks: open a window on a scratch `.pen`, then pipe requests into
`study mcp` (`cargo run -p pen --example mkreq -- <file.pen> snippet.js...` builds the JSON lines).
`study ipc debug_frame '{"camera":[x,y,zoom],"theme":"dark","waitMs":300}'` returns the most
recently focused window's frame as PNG (in `%TEMP%\study-ipc-1.png`) with its draw time.

## Rules

- Never break byte-exact round-trip of untouched content (see implementation notes: numbers, key
  order, `children` placement, line endings). Run the roundtrip example on real files after touching `pen::json`/`doc`.
- Test on copies of the user's `.pen` files, never the originals.
- The user's MCP server name is `pencil` (replacing Pen's), so tool names must keep matching Pen's.
- A running `study.exe` locks the binary: don't kill the user's windows to rebuild — rename the
  locked exe (`mv study.exe study.old.exe`), then build/install.
- The PowerShell tool has hung on this machine; prefer Bash (call `powershell.exe ... < /dev/null` if needed).
  JSON written through Bash heredocs can lose `\\` escapes — use forward slashes in Windows paths.

## CI and releases

`.github/workflows/build.yml` builds and tests on macos-latest and windows-latest for every push.
Pushing a `v*` tag (matching `version` in the workspace `Cargo.toml`) also packages `Study.app`
(ad-hoc signed tarball) and `study.exe` (zip) into a GitHub release. Nothing is tested on a real
Mac by the agent — macOS-specific behavior is only verified to compile; ask the user to check it.
Tag builds set `STUDY_RELEASE=1`, which turns on the self-updater (`updater.rs`): it reads
`releases/latest` and downloads `study-macos-arm64.tar.gz` / `study-windows-x64.exe` — keep those
asset names stable. Test the update flow with `STUDY_UPDATE_FEED=<local feed.json>` (GitHub
release JSON whose asset URLs may be local paths) and `study ipc command '{"id":"update_install"}'`.
Release steps for the user are in the README ("Releasing").
