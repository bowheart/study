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
  the canvas. When Claude reads part of a design a scan line sweeps over it, and frames it is
  changing glow with a "Claude is working" badge until it goes quiet.
- **The whole format.** Components and imports from other files, variables and themes, `script`
  nodes that generate content, WebP images, mesh gradients, and shader fills including animated
  (`@time`), interactive (`@mouse`), textured, SDF and backdrop shaders.

macOS (Apple Silicon) and Windows.

## Install

### macOS

```bash
curl -fsSL https://github.com/bowheart/study/releases/latest/download/study-macos-arm64.tar.gz | tar -xz -C /Applications
```

This installs `/Applications/Study.app`. The app isn't signed with an Apple Developer ID, so
install it with the command above: files downloaded by a browser get quarantined and macOS refuses
to open them ("can't be checked for malicious software"). If that happens, run
`xattr -dr com.apple.quarantine /Applications/Study.app`.

### Windows

Download `study-windows-x64.zip` from the [latest release](https://github.com/bowheart/study/releases/latest),
extract `study.exe` somewhere permanent (for example `%LOCALAPPDATA%\Programs\study`), and add that
folder to your PATH. The exe is unsigned, so Windows SmartScreen asks once before the first run.

### Updating

Release builds update themselves. A few seconds after launch, and every six hours while open,
study checks [GitHub Releases](https://github.com/bowheart/study/releases) and downloads a newer
version in the background. When it's ready, a banner at the bottom of the window offers
**Install and Restart** (reopens the same file) or **Install on next launch**. **Check for
Updates…** (in the study menu on macOS, the File menu on Windows) checks right away. No signing
or license is involved: the app downloads the release itself, so macOS doesn't quarantine it.

Copies built from source don't self-update; pull and run `cargo install --path crates/study` again.
Version 0.1.0 predates the updater, so install the next release once with the commands above.

### From source

You need [Rust](https://rustup.rs) (on Windows, the default MSVC toolchain with Visual Studio
Build Tools). The first build downloads prebuilt Skia binaries and takes a few minutes.

```bash
git clone https://github.com/bowheart/study.git
cd study
cargo install --path crates/study
```

This puts `study` in `~/.cargo/bin` (on your PATH if you installed Rust with rustup). Run the
same command after pulling updates. On Windows a running `study.exe` can't be replaced; close
study first, or rename the old exe out of the way (a running exe can be renamed).

Optional Windows Start menu entry (also lets Claude's computer-use tools find the app):

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

On macOS with the app from a release, use the full path:

```bash
claude mcp add pencil --scope user -- /Applications/Study.app/Contents/MacOS/study mcp
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

On macOS use Cmd wherever the table says Ctrl.

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

## Releasing

1. Bump `version` under `[workspace.package]` in `Cargo.toml` (installed apps update only to a
   higher version).
2. Commit and push to `master`; CI builds and tests both platforms.
3. Tag and push the tag:

   ```bash
   git tag -a v0.2.0 -m "study 0.2.0" && git push origin v0.2.0
   ```

The tag's workflow run builds release binaries (with the self-updater enabled), packages
`Study.app` and `study.exe`, and publishes a GitHub release with generated notes. Installed copies
pick it up within six hours, or immediately via Check for Updates.

## Files

`~/.study` holds downloaded fonts and icon sets, window state (recent files, cameras, theme) and
your optional `styles/`. Deleting it is safe.

## Not supported

- Image generation and stock photos (`Generate` with `"ai"` / `"stock"`), and Pen's `browser` tool.
- Pen's manual editing tools, panels, in-app chat, presets, sharing, presenting and Figma import.
- Script nodes that use `Math.random()` produce the same *kind* of output as in Pen but not the
  identical shapes (the random seed differs).
- `imports`: Pen doesn't document how references into imported files are written. study accepts
  `alias:id` refs and `$alias:name` variables (and bare ids/names found in an import) — tell me if
  a file with imports renders with missing pieces. Pen's built-in `pencil:` libraries load only
  when Pen itself is installed on the machine.

study is an independent personal project and is not affiliated with Pen.
