# Implementation notes

How study works and the non-obvious decisions behind it. Pen's behavior was inferred from its
bundled MCP docs, its file format, and comparing renders; nothing of Pen's code is used.

## File format and round-trip (`crates/pen`)

`.pen` files are plain JSON (`{"version": "2.17", "children": [...], "variables": ..., "themes": ...}`),
despite Pen's MCP docs calling them encrypted. Requirement: loading and saving an untouched file
reproduces it byte-for-byte, and edits change only what they touch.

- `doc::Document` is an arena of `Node { props, parent, children, attached }`. Each node's own
  properties stay a `serde_json` map (`preserve_order`), with `children` kept as a `null`
  placeholder so the key's original position survives. Unknown properties are preserved untouched.
- `json::Writer` reproduces `JSON.stringify(v, null, 2)`. Numbers use JavaScript's shortest
  round-trip formatting (`js_number`), including JS's tie rule: when two shortest digit strings both
  round-trip, pick the lower one only if the exact decimal value is the midpoint (`even_tie`).
  serde's default formatting differs on values like `111.12234497070312`. Requires the
  `float_roundtrip` feature.
- CRLF vs LF and the trailing newline are detected on load and kept (`doc.format`).
- Saves are atomic: a hidden `.<name>.tmp` beside the file, then a rename. Hidden so directory
  scanners that skip dotfiles never pick up a half-written file.
- Edits go through transactions: `begin` → mutations (`insert`, `update` where `null` deletes a
  key, `delete`, `move_node`, `set_header`) → `commit` returns an undo log, or `rollback`.
  `undo(log)` applies a log and returns its inverse (which is the redo log). Deleted nodes stay in
  the arena detached so undo can reattach them. New ids are 5 random alphanumerics, inserted right after `type`.
- `take_changes()` reports touched nodes / structure / variables so the engine re-lays out only
  affected root frames.

## Scene compilation and layout

Each top-level node compiles into a `scene::RootScene`: a flat vector of `SNode`s with resolved
style (variables and themes applied, instances expanded) plus computed rects and bounds.

- Variables: a theme is one value per axis; the default theme is the first value of each axis. A
  node's `theme` prop overrides axes for its subtree. `$name` references resolve recursively;
  themed values pick the most specific full match.
- Instances (`type: "ref"`): the component subtree is cloned, root overrides applied, then
  `descendants` entries keyed by descendant id or by `nested/child` path. An override containing
  `type` replaces the node; `children` replaces its children. Instance children get path keys
  like `inst/child` (that's how `execute` addresses them).
- Legacy forms still found in files: stroke objects `{align, thickness, fill}`, `icon_font` nodes.
- Imports (`imports.rs`): the header's `imports` maps aliases to other files (relative, percent-
  encoded) or `pencil:<kit>.lib.pen` (read straight out of a local Pen install's `app.asar`). Every
  node of an import is pre-serialized as `alias:id` with nested refs and `$vars` rewritten to
  `alias:...`; refs that miss locally compile from that JSON. Variables are merged into `Vars` as
  `alias:name`. Pen's own reference syntax is undocumented; bare ids/names that exist in an import
  also resolve.
- Script nodes compile as empty containers (`Kind::Script`). After layout the engine runs each
  script with its laid-out size (`study::scripts`) and `graft`s the returned JSON in as children
  (no ids, so not addressable or selectable), then lays out again.
- Layout (`layout.rs`), Pen semantics: frames default to `layout: "horizontal"`, sizes default to
  fit content. Widths resolve first, then heights (text wrapping depends on width), then positions.
  `fill_container` children contribute 0 to a `fit_content` parent; if every child fills, the parent
  uses its `fit_content(N)` fallback (or 0). `justifyContent` start/center/end/space_between/space_around,
  `alignItems` start/center/end. Absolute (`layoutPosition`) and disabled children leave the flow.
  Text growth: `auto` (one line), `fixed-width` (wraps), `fixed-width-height`.
  Rotation is degrees counter-clockwise around the node's top-left (`Affine::of_node`).
- Text measurement is behind `TextMeasure` so `pen` has no Skia dependency; tests use a monospace stub.

## Rendering (`crates/render`)

- Conventions verified against real files: gradient rotation 0° runs top→bottom, rotation is
  counter-clockwise (90° = left→right); `center`/`size` are normalized to the node box.
  Shadow blur → sigma = blur/2 and default stroke alignment `inner` are educated guesses.
- Text: SkParagraph with `height_override` + half-leading for `lineHeight`. Paragraphs and
  measurements are cached; caches are invalidated when a font arrives.
- Fonts: Google Fonts css2 API *without* a browser user agent returns static TTFs; downloaded in
  the background into `~/.study/fonts`. An empty cache file means "known missing"; network errors
  are not cached. Layout reruns when fonts arrive.
- Icons: npm tarballs (lucide-static, feather-icons, @phosphor-icons/core) unpacked into
  `~/.study/icons`; SVG DOMs cached by (library, name, color). Material Symbols are font ligatures.
- Shaders: Pen's WebGL1 fragment shaders are translated to SkSL (`shaders.rs`): strip `precision`/`#`
  lines, rename `gl_FragCoord`/`gl_FragColor` (reserved in SkSL), wrap `main`, flip y, rename
  locals that shadow builtins (`float dot = ...`). Annotations: `@resolution`, `@time` (live clock,
  roots using it are redrawn continuously and never thumbnailed), `@mouse`, `@default`. `sampler2D`
  uniforms become child shaders with a `study_size_<name>` uniform and a `study_tex_<name>(uv)`
  helper replacing `texture2D` (y flipped, unpremultiplied); `textureSize` maps to the size uniform.
  `@sdf` is a half-float RGBA image from an exact distance transform of the shape (32-bit float
  textures don't work on the GPU backend). `@backdrop` shaders run as a backdrop image filter in
  device space; `study_to_local`/`study_from_local` matrices map between device and node space.
- Mesh gradients: Coons patches per grid cell (`draw_patch`), explicit handles or Catmull-Rom ones.
- WebP: the prebuilt Skia has no WebP codec; `images::decode` and `offscreen::encode` use image-webp
  (lossless encode).
- Performance: per-node bounds culling; level-of-detail below thresholds in `draw.rs`
  (`LOD_FLATTEN_PX` subtrees → one flat rect of their dominant color, `LOD_GREEK_PX` text → bars,
  1px slivers flattened). Root frames smaller than `THUMB_MAX_PX` on screen are drawn from cached
  GPU rasters at power-of-two scales (finer LOD inside thumbnails), built within a 14 ms/frame budget,
  LRU-capped at 384 MB, keyed by the scene's `stamp` (changes on relayout).
  Reference numbers: a generated 920K-node file loads in ~3 s; frames are 0.3–3 ms once thumbnails are warm.
- `Renderer::reveal` (root → node index → progress) and `tips` drive the pen draw-in animation;
  revealing roots skip thumbnails.

## The app (`crates/study`)

- `engine.rs`: one open document — doc, vars, compiled scenes, renderer, undo/redo stacks, selection,
  persistent JS context, failed snippets by `editId`. Shared by GUI windows and headless MCP.
  `refresh()` recompiles dirty roots (and roots instancing an edited component).
- `api.rs` + `prelude.js`: `execute` runs the snippet in QuickJS (rquickjs) wrapped in an IIFE,
  **sloppy mode** so `x = Insert(...)` creates globals that persist across calls (Pen relies on
  this). One transaction per call: on error everything rolls back, globals created by the snippet
  are removed, and the snippet is stored under `eN` for `editId` + find/replace repair. On success:
  commit, push undo, save, then screenshots/exports. JS calls into Rust via a single
  `__host(name, argsJson)` function; `Host::call` dispatches. 120 s interrupt deadline.
- `svg.rs`: `Generate(frame, "svg", markup)` — usvg normalizes the SVG; each path becomes a `path`
  node with geometry translated to its own box and an explicit `viewBox`, fitted into the frame
  (centered, aspect kept); gradients are re-expressed in Pen's normalized form; the frame's
  layout is set to `none`.
- `mcp.rs`: JSON-RPC over stdio, tool schemas mirror Pen's. Routing per call: a window that has the
  file open → forward over IPC; else launch a detached window for the file and wait for it to
  register (≤20 s); else (or with `STUDY_HEADLESS`) open the document in-process.
- `ipc.rs`: each window listens on 127.0.0.1 with a random port + token and writes
  `~/.study/instances/<pid>.json` (open file, focus time). Stale entries are pruned by pid.
- Headless build: everything window-related (`gui.rs`, `uitext.rs`, `updater.rs`, winit/glutin/muda/
  rfd/ureq) is behind the default `gui` feature. `--no-default-features` builds the MCP server
  alone (the Linux release): it never launches windows and refuses `study <file>`.
- `gui.rs`: winit + glutin GL surface wrapped as a Skia surface; muda native menu bar with
  accelerators: on macOS they are real key equivalents (winit never sees those keys); on Windows
  they only label the items, because nothing calls `TranslateAccelerator`, so winit handles the keys. Theme =
  light/dark/system (title bar, menu bar and canvas). State (recent files, per-file cameras, theme)
  in `~/.study/state.json`. The disk is polled ~1 s for external changes.
  Agent feedback: after `execute`, new path nodes are queued for the pen draw-in (`draw_in`), nodes
  read with `Get` get a scan sweep (`scan`, `draw_scan`), and root frames the call changed glow with
  a "Claude is working" badge (`working`, `draw_working`) for `WORK_HOLD` seconds after the last
  change. MCP has no "agent finished" signal, so idle time is the only cue.
- `updater.rs`: self-update for release builds. Checks GitHub `releases/latest` 10 s after launch and
  every 15 min, stages the asset in `~/.study/updates/<version>/`, and the window shows a banner. Install
  swaps in place: macOS renames `Study.app` aside and `ditto`s the new bundle in; Windows renames the
  running `study.exe` to `study.old-<pid>.exe` (allowed while running) and copies the new one in;
  then it relaunches with the same file. "Install on next launch" writes `pending.json`, applied at
  the start of `gui::run`. `cleanup()` removes old exes and staged versions on startup.
- Shutdown: `Gl`'s fields are ordered so Skia and GL objects drop before the window. The reverse
  order hung the process on exit (the GL driver tearing down a context whose window was gone).
- `shot.rs` / `export.rs` / `html.rs`: screenshots (≤1568 px edge), PNG/JPEG/PDF export, and
  HTML+CSS / Tailwind generation.
- Release builds use the Windows subsystem (no console); `mcp` and `ipc` attach to the parent console.

## Web viewer (`crates/web`)

- **Shape.** `pen` + `render` compiled to `wasm32-unknown-emscripten` (a bin crate, kept out of
  the workspace's `default-members`), drawing through a WebGL2 context the page creates.
  `viewer.rs` holds one document, a camera, selection/hover and the presence overlays, and
  exports C functions (`study_*`). `js/study-viewer.js` is the page-facing API (an
  `EventTarget`), and `crates/web/README.md` documents it for embedders. Everything runs on the
  main thread.
- **Skia.** rust-skia publishes a prebuilt wasm Skia only for `gl`+`svg`+`textlayout` *with*
  `webp`, so the web crate asks for exactly that, gated on emscripten. Native builds keep their
  own feature set and prebuilt binaries.
- **setjmp/longjmp.** That Skia (FreeType, libpng, libjpeg) was compiled for Emscripten's
  JavaScript SjLj: `invoke_*` imports plus `emscripten_longjmp`. rustc links with
  `-fwasm-exceptions`, and Emscripten refuses to combine the two. `src/sjlj.cpp` implements
  `invoke_*` and `emscripten_longjmp` in C++ on top of wasm exceptions. `build.rs` passes the file
  to em++ on the link line. If Skia starts using new signatures, the link fails with an
  assertion about "invoke_ functions". Add them to the generated list at the bottom of
  `sjlj.cpp` (`llvm-nm` on `libskia.a` lists them).
- **Network.** `render::net::get` is the one way the renderer downloads. Native builds use a
  thread plus ureq. On the web, requests queue up, the page drains them after each frame
  (`study_requests`), fetches them (through the `fetch` option), and answers with
  `study_fulfill`.
- **Fonts on the web.** `render/src/fonts/web.rs` mirrors a browser's web-font loading:
  - Google's per-script WOFF2 subsets load lazily by `unicode-range` for the characters each
    text uses (`Fonts::request_text`). Subsets are decoded with `woff2-patched`.
  - The base (`latin`) subset registers under the family name, the others as `Family#subset`.
    `Fonts::families` lists them, then the loaded fallback families, as the paragraph's family
    list.
  - A subset's typeface overrides its declared range once loaded (`covers`), because Noto Sans
    Math has no range at all and Symbols 2 claims ⌘ without having it.
  - Stylesheet arrivals count as font changes, so texts relayout and request the subsets they
    now know about.
  - The UI font (controls, frame titles, labels) is Inter's Latin and Latin Extended WOFF2
    subsets from `crates/web/fonts`, compiled in with `include_bytes!`. The page has no system
    fonts, and a downloaded Inter can't be found reliably by its internal family name. Each
    character uses the first subset that has it (`UiFont::runs`).
  - Warnings: `Fonts::take_warnings` (web build) reports document font cuts and subsets that
    failed, and fallback failures once. `Images::take_failures` and `Shaders::errors` are found
    while drawing, so `study_warnings` collects all three when the page asks, after each frame.
- **Files.** The page's files live in Emscripten's in-memory filesystem under `/doc/<path>`.
  - `study_file_refs` scans the document's JSON string literals for relative paths with file
    extensions (images, shader and script sources, `.pen` imports). It recurses into imported
    documents already present and returns the missing ones.
  - `load()` in JS fetches those through `readFile` before `study_load`, and repeats while
    imports reveal more.
  - `doc.path` is set to `/doc/<path>`, so `Imports::load`, image and shader resolution and
    script URIs all work unchanged. The disk caches in `~/.study` are in memory too, so they
    last one page load.
- **Scripts.**
  - `pen::script` holds the code wrapping (pencil object, seeded `Math.random`) and output
    parsing. Desktop evaluates it with QuickJS (`scripts.rs`); the web hands the wrapped code to
    the page (`study_script_jobs`).
  - The page runs it in `study-sandbox.html`: an `allow-scripts` iframe with an opaque origin and
    a killable worker. Results come back through `study_script_result`, and the next frame
    rebuilds roots with script nodes.
  - In this app's embedded browser a sandboxed iframe loaded by URL never ran its script, so the
    viewer falls back to `srcdoc` after 2 s.
- **Selection.** Hit-testing and node transforms are shared with the desktop
  (`RootScene::hit`/`transform` in `pen::scene`). Click semantics, Enter/Shift+Enter and the
  outline drawing mirror `gui.rs`. `study_node_info` gives ids, the tree and document/screen
  boxes, `study_node_json` the source JSON, and `study_snapshot` a PNG via
  `render::offscreen` (like the MCP screenshot tool).
- **Performance.** `load` reparses and relays out the whole document: 1 ms for the showcase,
  0.7 s for 230K nodes, 3.3 s for 920K. Frames keep up with a 165 Hz display while panning at
  920K nodes; zooming through mid levels averages ~17 ms while thumbnails rebuild. Incremental
  reloads (diffing roots) are the next step if large files change often.
- **Canvas controls.** The breadcrumb, zoom buttons and light/dark switch are `render::controls`,
  shared with the desktop window. Each frame, the viewer lays them out from the selection
  (`RootScene::ancestors`) and the camera, draws them last, and checks pointer input against
  them (`study_on_controls`/`study_press`) before the canvas sees it. `Show` turns each one off.
- **Layers.** A `Layer` is a document with its vars, imports and laid-out scenes. The viewer's
  `main` is the one shown; `overlay` is drawn over it inside `save_layer_alpha` (onion skin),
  and `other` holds a version `compare` is waiting on. All of them share the renderer, so fonts
  and images load once. `Renderer::layer` keeps their cached thumbnails apart, since root ids
  repeat across documents. Script results and arriving fonts rebuild every layer (`pump`).
- **Compare.** `pen::diff::compare` matches scene nodes by key (instance paths inside
  instances). Properties come from the node's own JSON, or from `instance::expand` for instance
  nodes, so component edits show where they render. `moved` is a parent change, or not being in
  the longest run of siblings that kept their order. `bounds` counts only boxes that changed
  relative to their parent. `compare` waits for `settling()` (font downloads, script runs)
  through `study_pump`, not frames, because background tabs don't get animation frames.
- **Changes layout** (`crates/web/src/changes.rs`, `showChanges`). Rows live in *list space*:
  CSS px at zoom 1, as wide as the canvas. The camera moves over list space instead of the
  document, as freely as over a document. Each side of a row is a `Place`: a top-level root of the
  main layer or the `before` layer, its document box, and where it's drawn. `draw_rows` clips to
  the cell and draws the real scene through that transform, so nothing is copied or re-laid out
  per row. `doc_xf(side, key)` (document → screen) is what every overlay uses: selection, hover,
  highlights, `NodeInfo.screen`, agent presence. Selection and hover carry a `Side`. Rows are
  recomputed (`diff`) on load, script results and fonts. They're re-placed (`place`) on resize
  and new options, and `version` bumps when geometry changes (the page's `layoutchange`). The
  `before` layer draws with `Renderer::layer = 2`. `Changes::highlight_alpha` scales the marks:
  1 with `highlights` on, else a `flashHighlights` hold and fade (frames continue while it runs).
- **MCP for host apps.** Headless `study mcp` with `STUDY_EVENTS=<file>` appends one JSON line
  per execute call (`record_presence`). The line carries `size`/`sha256` of the bytes the call
  saved (`Engine::saved`, from the text `Document::save_as` returns) and `created` (set by
  `Engine::open`). `STUDY_SELECTION=<file>` (`host_view`) names the document and selection the
  user has in the page, and headless `get_app_state` reports them like a window does.
- **Confinement for servers.** `pen::access` reads `STUDY_ROOT` and `STUDY_WRITABLE` once and
  is called at every file read and write: `Document::load`/`save_as` (documents and imports),
  `Images::get`, `Shaders::shader`, `Scripts::run`, `Engine::open` and `export`. `resolve`
  follows symlinks component by component, including for paths that don't exist yet, and fails
  on broken links, so a link can't point a write outside. A document outside `STUDY_WRITABLE`
  gets `Engine::read_only`. `execute` rolls a call back if it changed such a document
  (`Document::has_pending_changes`), and `Engine::save` refuses it. `render::net` checks
  `STUDY_FETCH_HOSTS` and follows redirects itself so each hop is checked.
  `render::data_dir` honors `STUDY_DATA_DIR`.
- **Build environment.** `build.sh` puts the SDK's own Python and Node on `PATH` and links with
  `em++`, because rustc's default `emcc.bat` is gone from newer SDKs.

## Known gaps

Image generation / stock photos, the `browser` tool, exact parity of seeded randomness in scripts.
macOS is built and tested in CI but has not been exercised by an agent on a real Mac. Memory is ~2.5 GB for 920K nodes (per-node JSON maps) — the
first thing to optimize if huge files become common.
