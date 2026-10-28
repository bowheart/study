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
- Saves are atomic (`.pen.tmp` + rename).
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
  locals that shadow builtins (`float dot = ...`). Uniform annotations `@resolution`, `@time`,
  `@default` are parsed from doc comments. `@time` is fixed at 0.
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
- `gui.rs`: winit + glutin GL surface wrapped as a Skia surface; muda native menu bar (no muda
  accelerators — keys are handled in winit and the shortcut text is in the labels). Theme =
  light/dark/system (title bar, menu bar and canvas). State (recent files, per-file cameras, theme)
  in `~/.study/state.json`. The disk is polled ~1 s for external changes.
  Agent feedback: after `execute`, new path nodes are queued for the pen draw-in (`draw_in`) and
  nodes read with `Get` get a scan sweep (`scan`, `draw_scan`).
- `shot.rs` / `export.rs` / `html.rs`: screenshots (≤1568 px edge), PNG/JPEG/PDF export, and
  HTML+CSS / Tailwind generation.
- Release builds use the Windows subsystem (no console); `mcp` and `ipc` attach to the parent console.

## Known gaps

`imports` of other `.pen` files, `script` nodes, WebP, animated `@time` shaders, image generation /
stock photos, the `browser` tool. Memory is ~2.5 GB for 920K nodes (per-node JSON maps) — the
first thing to optimize if huge files become common.
