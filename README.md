# study

A fast, local canvas for [Pen](https://pen.dev) `.pen` design files, built for Claude to design on.

study opens your existing `.pen` files exactly as they are, renders them the way Pen does, and
gives Claude Code the same MCP tools Pen does (`get_app_state`, `execute`, `read_skill`,
`get_style`). It skips the manual editing UI: you pan, zoom and select; Claude does the designing.

<p align="center"><img src="assets/icon-256.png" width="96" alt="study logo"></p>

- **Same files.** Opens schema 2.17 (and older 2.11) files. Saving keeps everything study didn't
  touch byte-for-byte identical, so files move freely between study and Pen.
- **Same tools.** Register study under the name `pencil` and every prompt, skill and permission
  you had for Pen keeps working.
- **Faithful rendering.** Skia (the engine Pen's renderer uses), Pen's flex layout, variables and
  themes, components and overrides, gradients, images, GLSL shader fills, shadows and blurs,
  Lucide/Feather/Phosphor/Material icons and any Google Font.
- **Big documents.** Off-screen culling, level-of-detail and cached thumbnails keep frames in the
  low milliseconds even with ~1M nodes.
- **No app babysitting.** Claude can edit any `.pen` file whether or not study is open; study
  opens a window for it, and everything is saved as it goes.
- **Drawing.** Claude can draw vector art (logos, illustrations) and you watch a pen trace it onto
  the canvas. When Claude reads part of a design, a scan line sweeps over it.

Windows only for now.

## Install

You need [Rust](https://rustup.rs) with the default MSVC toolchain (Visual Studio Build Tools).
The first build downloads prebuilt Skia binaries and takes a few minutes.

```bash
git clone https://github.com/bowheart/study.git
cd study
cargo install --path crates/study
```

This puts `study.exe` in `~/.cargo/bin` (already on your PATH if you installed Rust with rustup).
Run the same command after pulling updates. If study is running, Windows won't let the file be
replaced; close study first, or rename the old `study.exe` out of the way (a running exe can be renamed).

Optional Start menu entry (also lets Claude's computer-use tools find the app):

```powershell
$s = (New-Object -ComObject WScript.Shell).CreateShortcut("$env:APPDATA\Microsoft\Windows\Start Menu\Programs\study.lnk"); $s.TargetPath = "$env:USERPROFILE\.cargo\bin\study.exe"; $s.Save()
```

## Connect Claude Code

### Replacing Pen (recommended)

Pen registers itself in Claude Code as an MCP server named `pencil`. Point that name at study:

```bash
claude mcp remove pencil --scope user
```

```bash
claude mcp add pencil --scope user -- study mcp
```

Tools then appear as `mcp__pencil__execute` and so on, exactly as before, and an existing
`mcp__pencil` permission rule covers them. Restart open Claude sessions to pick up the change.

Opening Pen again may re-register its own `pencil` server over yours. If Claude starts talking
about plan limits or encrypted files, run the two commands again.

### Alongside Pen

```bash
claude mcp add study --scope user -- study mcp
```

This works, but Claude then sees two near-identical tool sets and may pick the wrong one.

### How Claude uses it

- Every `execute` call names a `.pen` file. If a study window has it open, the change shows up
  there live. Otherwise study opens a window for it — you don't need to open files first.
- Set `STUDY_HEADLESS=1` in the MCP server's environment to never open windows; screenshots and
  exports still work.
- Each successful call is saved immediately and is one undo step (Ctrl+Z in the window).
- `Generate(frame, "svg", markup)` is how Claude draws vector art. Pen sends a prompt to its own
  image service; study has none, so Claude writes the SVG itself and study turns it into path
  nodes. Image generation and stock photos (`"ai"`, `"stock"`) aren't available.
- `get_style` serves style guides you write as markdown files in `~/.study/styles/`.

## Using the window

```bash
study path/to/design.pen
```

| Input | Action |
| --- | --- |
| Middle-drag, or Space + drag | Pan |
| Wheel / Shift + wheel | Scroll vertically / horizontally |
| Ctrl + wheel (or pinch) | Zoom at the cursor |
| Ctrl+0 / Ctrl+1 / Ctrl+2 | Fit everything / 100% / zoom to selection |
| Click, Shift+click | Select (Claude sees your selection) |
| Shift+Enter / Enter | Select parent / first child |
| Esc | Clear selection |
| Ctrl+Z / Ctrl+Shift+Z | Undo / redo Claude's last change |
| Ctrl+N / Ctrl+O, or drop a file | New / open (File › Open Recent for recent files) |
| Ctrl+S / Ctrl+Shift+S | Save / save as |
| F5 | Reload from disk |
| B, or the sun/moon switch | Light / dark canvas (View › Theme can follow Windows) |

Changes made by other programs are picked up automatically. Avoid having the same file open in
Pen and study at once: Pen may overwrite study's changes when it saves.

[examples/showcase.pen](examples/showcase.pen) is a sample dashboard Claude built in study.

## Files

`~/.study` holds downloaded fonts and icon sets, window state (recent files, cameras, theme) and
your optional `styles/`. Deleting it is safe.

## Not supported

- Image generation and stock photos (`Generate` with `"ai"` / `"stock"`), and Pen's `browser` tool.
- Pen's manual editing tools, panels, in-app chat, presets, sharing, presenting and Figma import.
- `script` nodes (drawn as placeholders), `imports` of other `.pen` files, WebP images and exports.
- Animated shaders (`@time` uniforms) render as a still frame.

study is an independent personal project and is not affiliated with Pen.
