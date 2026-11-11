# study web viewer

This crate puts study's canvas in a web page. It's the same Rust layout and Skia renderer as the
desktop app, compiled to WebAssembly and drawn with WebGL2, so `.pen` files look the same as on
desktop.

The viewer supports:

- **Viewing:** documents with imports, `script` nodes, images, shaders and every font script.
- **Interaction:** selection, hover, and the desktop app's keys, plus events your app can hook.
- **Agent presence:** the scan, the "Claude is working" glow, and the pen drawing paths in, as an
  agent works on the file on your server.

There's no editing, panels or undo. Changes come from agents through the `pencil` MCP tools.

## Build and install

```bash
crates/web/build.sh        # needs the Emscripten SDK (5.0+) in ~/emsdk or $EMSDK
```

Or skip the build: releases include `study-web.zip` with the same files, built by CI.

Copy these four files from `crates/web/dist/` (or the zip) into one folder in your app's static
files:

| File | What it is |
| --- | --- |
| `study-viewer.js` | The API (an ES module) |
| `study-web.js` | Emscripten's loader, imported by `study-viewer.js` |
| `study_web.wasm` | The renderer, about 3.6 MB gzipped |
| `study-sandbox.html` | The page `script` nodes run in |

Bundlers that understand `new URL(..., import.meta.url)`, such as Vite, find the files on their
own. Otherwise pass `locateFile: (name) => "/static/study/" + name`. Serve `dist/` to try the
demos: `demo.html` shows every feature, `compare.html` compares two versions side by side, and
`changes.html` shows only the frames that changed.

## Showing a document

```js
import { createViewer } from "/static/study/study-viewer.js";

const viewer = await createViewer(canvas, {
  theme: "system",                    // "light" | "dark" | "system"
  readFile: (path) => api.readSpecFile(path), // see "Files, imports and scripts"
});
await viewer.load(penText, { path: "specs/home.pen" });
```

- **`canvas`:** size it with CSS. The viewer follows its size and the device pixel ratio.
- **`load(pen, { path })`:** takes JSON text, a parsed object or bytes, and rejects if the
  document is invalid. `path` is where your app keeps the file. Relative references resolve
  from its folder.
- **Reloading:** call `load` again whenever the file changes. The camera and selection stay put,
  and selected nodes that no longer exist are dropped.
- **`destroy()`:** call it when the canvas goes away.

Input works like the desktop app:

| Input | Action |
| --- | --- |
| Scroll / Shift+scroll | Pan |
| Ctrl/Cmd+scroll, trackpad pinch | Zoom at the pointer |
| Drag (left or middle button) | Pan |
| Click / Shift+click | Select / add to selection |
| Enter / Shift+Enter | Select first child / parent |
| Esc | Clear selection |
| Ctrl/Cmd+0 / 1 / 2 | Fit everything / 100% / zoom to selection |

Keys work while the canvas has focus, which it gets on click. Pass `interactive: false` to turn
off selection, hover and keys and keep only pan and zoom.

## Built-in controls

The canvas draws the same controls as the desktop app:

- **Breadcrumb**, top left: shows the selected node and each of its ancestors. Click one to
  select it, which is how you get from a small element up to its section or the whole frame.
  Long paths collapse their middle into "…".
- **Zoom**, bottom right: zoom out, the current percentage (click it to fit everything), and zoom
  in.
- **Light/dark switch**, bottom right: clicking it fires `themechange`.

The controls, frame titles and selection labels are set in Inter's Latin and Latin Extended
subsets, which are built into the viewer, so they draw immediately and don't depend on Google
Fonts. Inter is under the SIL Open Font License; `Inter-OFL.txt` ships next to the viewer.

Turn any of them off, at creation or later, and drive the same behavior from your own UI:

```js
const viewer = await createViewer(canvas, { controls: { theme: false } }); // the app owns the theme
viewer.setTheme(user.prefersDark ? "dark" : "light");
viewer.setControls({ breadcrumb: false, zoom: false });
```

| Built-in control | Do it yourself with |
| --- | --- |
| Breadcrumb | `viewer.breadcrumb` (`NodeInfo[]`, top-level frame first) on `select`. Like the built-in one, swap only the last selected node for the clicked one: `viewer.select([...ids.slice(0, -1).filter((id) => id !== node.id), node.id], node)`, where `ids` is `viewer.selection.map((n) => n.id)` |
| Zoom | `viewer.zoom` on `viewchange`; `viewer.zoomBy(1.25)`, `viewer.zoomBy(0.8)`, `viewer.fit()` |
| Theme | `viewer.setTheme("light" \| "dark" \| "system")`, `viewer.theme` |

## Selection, clicks and a toolbar

Clicking selects the deepest node under the pointer, Shift+click adds to the selection, and a
click on a top-level frame's title (drawn above it) selects the whole frame.

Every node is described by a `NodeInfo`:

```ts
{
  id: string;          // node id, or "instanceId/childId" inside a component instance
  name: string | null;
  type: string;        // "frame", "text", "ref", "script", ...
  parent: string | null;
  root: string | null; // the top-level frame it's in
  instance: string | null; // the component instance it's inside, if any
  side: "after" | "before"; // "before": the older version in the changes layout
  bounds: { x, y, width, height }; // document units
  screen: { x, y, width, height } | null; // CSS pixels from the canvas's top-left corner
  // (null in the changes layout when its frame has no row)
}
```

| Member | |
| --- | --- |
| `viewer.selection` | `NodeInfo[]` |
| `viewer.select(ids, { side })` | Replace the selection (`[]` clears it) |
| `viewer.node(id, { side })` / `viewer.nodeAt(x, y)` | `NodeInfo` or `null` |
| `viewer.nodeJson(id, { side })` | The node as written in the `.pen` file |
| `viewer.snapshot(id, { scale, side })` | A PNG `Blob` of the node as drawn |
| `viewer.zoomTo(ids)`, `fit()`, `zoomBy(f)`, `camera` | View control |
| `viewer.compare(pen)`, `showChanges(pen)`, `highlight(ids, { kind })`, `setOverlay(pen)` | See "Comparing versions" |

Events come from `viewer.addEventListener(type, (e) => …)`, with data in `e.detail`:

| Event | Detail | When |
| --- | --- | --- |
| `select` | `{ nodes }` | The selection changed |
| `nodeclick` | `{ node, x, y, event }` | A click, not a drag. Fires before selecting; `e.preventDefault()` keeps the selection |
| `nodecontextmenu` | `{ node, x, y, event }` | Right-click. The node is selected first. `e.detail.event.preventDefault()` hides the browser menu |
| `hover` | `{ node }` | The node under the pointer changed |
| `viewchange` | `{ camera }` | The view moved; re-read `node(id).screen` |
| `warning` | `{ message }` | A script, import, image, shader or font failed to load or run (see below) |
| `themechange` | `{ theme }` | The built-in light/dark switch was clicked (`"light"` or `"dark"`) |
| `layoutchange` | `{ rows }` | The changes layout's rows changed (see "Showing only what changed") |

For example, a toolbar that follows the selection:

```js
const place = () => {
  const [node] = viewer.selection;
  toolbar.hidden = !node;
  if (!node) return;
  // The canvas and toolbar share a positioned parent.
  toolbar.style.left = `${node.screen.x + node.screen.width / 2}px`;
  toolbar.style.top = `${node.screen.y}px`;
};
viewer.addEventListener("select", place);
viewer.addEventListener("viewchange", place);
```

### Telling the agent what the user picked

Agents edit through study's `pencil` MCP tools, which address nodes by `id`. A node's `id` plus
the `.pen` file's path is all an agent needs. It reads the rest with `Get(id)`. There are two
ways to pass that along, and they work well together:

1. **In the message.** Send the selection with the user's question. It can hold several nodes
   (Shift+click adds to it), so send all of them, for example:

   ```js
   const nodes = viewer.selection;
   const context = {
     file: "/srv/specs/home.pen",       // the path the agent's MCP server opens
     nodes: nodes.map((node) => ({
       node,                            // id, name, type, place in the tree
       json: viewer.nodeJson(node.id),  // what the node contains
     })),
   };
   // Optional: what they look like (PNG Blobs).
   const images = nodes.map((node) => viewer.snapshot(node.id));
   ```

2. **As the app's state.** When the agents' MCP server runs with
   `STUDY_SELECTION=/path/view.json`, keep that file up to date:

   ```json
   {"file": "/srv/specs/home.pen", "selection": ["ZcurH"]}
   ```

   `get_app_state`, the first call an agent makes, then reports that document and selection the
   same way the desktop app reports its own. Use one file per agent session.

## Comparing versions

To show what changed between two versions of a file, put them in two viewers side by side and
let the newer one compare itself with the older:

```js
const before = await createViewer(leftCanvas, { readFile });
const after = await createViewer(rightCanvas, { readFile });
await before.load(oldText, { path });
await after.load(newText, { path });

const diff = await after.compare(oldText, { path });
after.highlight(diff.added, { kind: "added" });                  // green
after.highlight(diff.changed.map((c) => c.id), { kind: "changed" }); // amber
before.highlight(diff.removed, { kind: "removed" });             // red

// One camera for both.
let syncing = false;
const link = (a, b) => a.addEventListener("viewchange", (e) => {
  if (syncing) return;
  syncing = true;
  b.camera = e.detail.camera;
  syncing = false;
});
link(before, after);
link(after, before);
```

`dist/compare.html` is this, with a list of the changes. Serve `dist/` to try it.

**`compare(before, { path, timeout })`** resolves to what changed from `before` to the document
the viewer shows, as laid out and rendered:

| Field | |
| --- | --- |
| `added` | Ids only in the shown document. Only the outermost node of a new subtree is listed |
| `removed` | Ids only in `before`, likewise. Highlight them in a viewer showing `before` |
| `changed` | `{ id, props, moved, bounds }` for nodes in both that differ (below) |
| `frames` | Top-level nodes containing any of these, for a "frames changed" summary |

Each entry in `changed`:

- **`props`:** the names of properties whose values differ, such as `["fill", "padding"]`.
- **`moved`:** true if it has a different parent, or a different order among siblings that are
  in both versions. When one node moves to the end of a list, only that node is `moved`.
- **`bounds`:** `{ before, after }` boxes in document units, when it moved within its parent or
  changed size, even if its own properties didn't change. A sibling pushing it down, flex giving
  it more room, or a component change resizing it all show up here. Children that only moved
  along with their parent aren't listed.

A JSON diff misses some of what these catch:

- **Matching by instance path:** nodes match by id, and inside component instances by instance
  path (`instanceId/childId`).
- **Instances compared as they render:** editing a component reports a change on every instance
  where it shows. An instance that overrides that property doesn't change, so it isn't reported.
- **Script output left out:** nodes that `script` nodes generate have no stable ids, so they're
  skipped. The `script` node itself is compared.
- **Waiting for fonts and scripts:** `compare` waits up to `timeout` ms (default 10000) for fonts
  and `script` nodes, so text measures the same in both versions. It usually takes milliseconds
  once fonts are in, and works in a background tab.
- **Files:** `path` is where `before` is kept, for its relative files, and defaults to the shown
  document's path. Its files are read through `readFile` like `load`'s.

**`highlight(ids, { kind })`** outlines and tints nodes, apart from selection and hover:

- **Kinds:** `kind` is `"added"`, `"changed"` (the default), `"removed"` or a `#rrggbb` colour.
- **One set per kind:** calling again with the same kind replaces that set.
- **Clearing:** `highlight([])` clears every kind; `highlight([], { kind })` clears one.

**Onion skin:** `setOverlay(pen, { path, opacity })` draws another document over the shown one,
translucent, to see exactly what shifted. `viewer.overlayOpacity = 0.3` changes the opacity
without reloading it, for a slider, and `setOverlay(null)` removes it. The overlay isn't
interactive: selection and hover stay on the shown document.

### Showing only what changed

Frames in a `.pen` file sit anywhere on the canvas, so a review is easier when it shows just the
frames that changed, lined up. `showChanges` turns a viewer into that list: one row per changed
top-level frame, the older version on the left and the shown one on the right, at the same scale.

```js
await viewer.load(newText, { path });
const view = await viewer.showChanges(oldText, { path, gap: 44 });

for (const row of view.rows) addHeader(row); // your HTML: name, kind, what changed
viewer.addEventListener("viewchange", placeHeaders);   // scrolling moves the rows
viewer.addEventListener("layoutchange", placeHeaders); // rows resized or recomputed
list.onclick = (id) => view.scrollToRow(id);

await viewer.showChanges(null); // back to the whole file, where the camera was
```

- **One renderer, real documents.** Both versions load fully, once, in the viewer you already
  have: variables, themes, components, imports, fonts, images and scripts resolve as in a normal
  load, and ids are the documents' own. Only the rows on screen are drawn, so a file with
  hundreds of frames and four changes costs about what those four frames cost.
- **Rows.** By default, every frame `compare` reports, in document order (a removed frame where
  it used to be). `frames: [ids]` picks them instead. A frame only in the newer version leaves the
  left side empty, and one only in `before` leaves the right side empty.
- **Sizes.** The viewer sizes the rows: each side gets half the width, minus `padding` (24) and
  `gutter` (24), and a row is as tall as its taller side. `fit: "width"` (the default) fills the
  column width; `fit: "contain"` also keeps each row within the canvas height. `maxRowHeight`
  scales down taller rows.
- **Highlights.** Added and changed nodes are outlined on the right, removed ones on the left, in
  the `highlight` colours (`colors: { added, changed, removed }` overrides them with `#rrggbb`).
  A frame is tinted as a whole when it was added or removed, or when its own properties changed.
  If it only grew or moved, the row's `kind` and `props` say so instead. `highlights: false`
  hides the outlines and tints (rows, selection and hover stay), and
  `view.flashHighlights({ duration })` shows them for `duration` ms (1000), then fades them, for
  a "what changed?" button next to a toggle.
- **One camera.** Scrolling and zooming move all rows together, freely, as on a document.
  `fit()` and the zoom control's percentage go back to 100% at the current scroll position.
- **Interaction.** Hover and selection work on both sides. `NodeInfo.side` says which version a
  node is in (`"before"` or `"after"`), and the selection is on one side at a time. On the
  `before` side, pass `{ side: "before" }` to `node`, `nodeJson`, `snapshot` and `select`.
- **Onion skin.** `mode: "onion"` draws each row as one column, the older frame over the newer at
  `opacity` (0.5). `viewer.overlayOpacity` changes it; selection stays on the newer version.
  `view.setOptions({ mode: "side" })` switches back without reloading anything.
- **Live.** It stays until `showChanges(null)`. `load` of a newer version recomputes the rows.
  Fonts or scripts arriving and the canvas resizing re-size them. Each fires `layoutchange`.

`showChanges` resolves once fonts and scripts have settled, or after `timeout` ms (10000), like
`compare`. Each entry in `view.rows`:

| Field | |
| --- | --- |
| `id`, `name` | The top-level frame |
| `kind` | `"added"`, `"changed"` or `"removed"`; `"unchanged"` for a frame in `frames` that didn't change |
| `props`, `moved` | The frame's own changed properties, and whether its order changed (as in `compare`) |
| `before`, `after` | `{ x, y, width, height }` where each version is drawn, in CSS px from the canvas's top-left, or `null` |
| `bounds` | The whole row |

`view.rowAt(x, y)` returns the row at a point (gaps included) with the `side` it's on.
`view.setOptions({...})` changes any option above without reloading `before`. `view.options`
and `view.mode` read them back. `viewer.changes` is the current view, or null.
`dist/changes.html` shows it with HTML row headers, a list of rows, the onion switch, and a
highlights toggle with a Flash button.

## Showing an agent's work live

Run the agents' MCP server headless, logging what each call did:

```bash
STUDY_HEADLESS=1 STUDY_EVENTS=/srv/specs/events.jsonl STUDY_SELECTION=/srv/specs/view.json study mcp
```

Each `execute` call that reads, changes or creates something appends a line to the events file:

```json
{"file": "deliverables/home.pen", "time": 1794241048796, "read": ["ZcurH"], "edited": ["ZcurH"], "drawn": ["HYOgq"], "size": 48213, "sha256": "9f2c…"}
```

| Field | |
| --- | --- |
| `file` | Relative to `STUDY_ROOT` when that's set (with `/`), otherwise the `filePath` the agent passed |
| `read` | Nodes the call read: give them a scan sweep |
| `edited` | Top-level frames it changed: they glow |
| `drawn` | Path nodes it created: the pen draws them in |
| `size`, `sha256` | Present when the call saved the file: the exact bytes it wrote |
| `created` | `true` when the call created the file |

Calls that only read have no `size` or `sha256`, since the file didn't change. Push the saved file
and the event to the page, over your WebSocket or server-sent events. Load the file first, then
replay the event. When two saves land close together, the hash tells you whether the bytes you
loaded are the ones the event describes:

```js
const bytes = new Uint8Array(await (await fetch(fileUrl)).arrayBuffer());
const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
const hex = [...digest].map((b) => b.toString(16).padStart(2, "0")).join("");
if (!event.sha256 || hex === event.sha256) {
  await viewer.load(bytes, { path: event.file });
  viewer.working(event.edited); // top-level frames glow ("Claude is working") for ~20 s
  viewer.drawIn(event.drawn);   // new paths are drawn in by the pen
  viewer.scan(event.read);      // nodes it read get a scan sweep
}
```

`load` takes 1 ms for a typical file, 0.7 s for 230K nodes and 3.3 s for 920K.

Saves write a hidden `.<name>.tmp` beside the file and rename it into place, so a directory
scanner that skips dotfiles never sees a half-written file.

### On a Linux server

Releases include `study-linux-x64.tar.gz` and `study-linux-arm64.tar.gz`, built on Ubuntu 24.04,
so they need glibc 2.39 or newer: Ubuntu 24.04, Debian 13 and later. (The Skia library study
uses ships binaries that need it.) For Docker, base the image on `ubuntu:24.04` or
`debian:trixie`. Each is the MCP server alone, with no window, menus or updater. It never opens
windows, so it doesn't need `STUDY_HEADLESS`.

- **Dependencies:** it needs fontconfig (`apt install libfontconfig1`).
- **Fonts for screenshots:** screenshots use the system's fonts for characters a document's
  fonts lack, so a font package such as `fonts-noto` helps.
- **Building it yourself:**

  ```bash
  cargo build --release -p study --no-default-features   # target/release/study
  ```

Point each agent session's MCP config at it as the `pencil` server, with the environment
variables below.

### Limiting what agents can touch

When agents work on files other people uploaded, give each `study mcp` process these settings.
All of them are off unless set.

| Variable | Effect |
| --- | --- |
| `STUDY_ROOT=<dir>` | Every file study reads or writes must resolve inside `<dir>`, after following symlinks: `filePath`, `imports`, image, shader and `script` sources, and `Export` output. Anything else is refused, and images outside it render blank. |
| `STUDY_WRITABLE=<dir>[:<dir>...]` | Study saves, creates `.pen` files and exports only inside these directories (separated like `PATH`). Documents elsewhere open read-only. |
| `STUDY_DATA_DIR=<dir>` | Where downloaded fonts and icon sets are cached, instead of `~/.study`. Point it at a persistent volume. |
| `STUDY_FETCH_HOSTS=<host>,...` | The only hosts study may contact; `*.example.com` covers subdomains. Each redirect is checked too. An empty value blocks all network access. |

Relative directories are resolved against the process's working directory. For the layout
`context-files/` (uploads, read-only), `deliverables/` (agent output) and `.study/` (yours), run
each agent turn in the workspace with:

```bash
STUDY_ROOT=. STUDY_WRITABLE=deliverables \
STUDY_EVENTS=.study/events.jsonl STUDY_SELECTION=.study/view.json \
STUDY_DATA_DIR=/var/cache/study \
STUDY_FETCH_HOSTS=fonts.googleapis.com,fonts.gstatic.com,registry.npmjs.org \
study mcp
```

- **Fonts and icons:** that allowlist covers them. Image URLs in documents then fail, so a
  document someone uploads can't make the server request other URLs.
- **Read-only documents:** a snippet that only reads works as usual. One that would change the
  document fails, leaving the document and file untouched, with an error such as
  `context-files/home.pen is read-only. Copy it into deliverables/ to edit it.`
  `get_app_state` says so too.
- **Creating files:** an `execute` on a path that doesn't exist creates the file only inside a
  writable directory. Elsewhere it fails with `<path> can't be created there`.
- **Error messages** name files relative to `STUDY_ROOT`, so agents don't see server paths.
- **Proxies:** `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY` and `NO_PROXY` are honored, with or
  without these settings.

## Files, imports and scripts

A document can refer to files by relative path: image fills, shader sources, `script` node
sources and `imports` of other `.pen` files. Before `load` lays a document out, it asks
`options.readFile(path)` for each file it doesn't have yet. Paths are relative to your app's root
and resolved from the document's `path`. Return a `Uint8Array`, `ArrayBuffer` or string, or
`null` if the file doesn't exist. Imported documents are scanned for their own files too.

`viewer.addFile(path, data)` provides or replaces a file directly, for example after the agent
regenerates an image. Without `readFile`, files can only arrive through `addFile`.

A file the document uses but the viewer doesn't have produces a `warning` when it's needed, for
example `assets/hero.png: the page didn't provide this image (see readFile)`. Images and shaders
are read when they're first drawn, so their warnings can come after `load` resolves. Images that
fail to decode or download warn too. `readFile` may also be asked about strings that only look
like file names (a layer called `logo.svg`), so a `null` answer on its own doesn't warn.

`script` nodes run their JavaScript in `study-sandbox.html`, inside an iframe with
`sandbox="allow-scripts"`. That gives scripts an opaque origin: no access to your app's cookies,
storage, DOM or same-origin APIs. Each script runs in a worker that's killed after
`scriptTimeout` ms (default 5000). They run the same wrapped code as the desktop app, including
its seeded `Math.random`, so the output matches.

Pen's built-in `pencil:` libraries come from Pen's own app files, so they aren't available.

## Fonts

Documents name Google Fonts families, as in Pen. The viewer loads them the way a browser loads
web fonts, then covers whatever is left with fallback fonts, much as the desktop app falls back
to the system's fonts:

1. **Stylesheet:** for each family, weight and style a text uses, it fetches Google's stylesheet:
   `https://fonts.googleapis.com/css2?family=<Family>:wght@<weight>`, or `ital,wght@1,<weight>`.
   For browsers, Google splits each font into subsets by script (`latin`, `cyrillic`, `greek`,
   `vietnamese`, numbered CJK slices, ...), each with a `unicode-range`.
2. **Subsets:** it downloads only the subsets whose ranges cover characters the text actually
   uses, from `fonts.gstatic.com`. Latin always loads. Google sends WOFF2, or WOFF to some browsers
   (Safari, Firefox); the viewer decodes both. A
   subset's real glyph table overrides its declared range once loaded, because the range can
   claim characters the file lacks.
3. **Fallbacks:** characters the family doesn't have at all (emoji, symbols, CJK in a Latin
   font, ...) go down a list of fallback families, loaded the same lazy way. The default list,
   in order:
   - Noto Sans, Symbols, Symbols 2, Math, Noto Color Emoji
   - Noto Sans SC, TC, JP, KR
   - Noto Sans Arabic, Hebrew, Devanagari, Bengali, Tamil, Thai, Armenian, Georgian, Ethiopic

   Han characters use the first CJK family in the list. If your users mostly write Japanese,
   for example, put `"Noto Sans JP"` first:

   ```js
   createViewer(canvas, { fallbackFonts: ["Noto Sans", "Noto Color Emoji", "Noto Sans JP", "Noto Sans SC"] });
   ```

4. **Missing families:** a family Google doesn't have (HTTP 400) is rendered entirely with the
   fallbacks.
5. **Warnings:** each family, weight or subset of the document's fonts that can't be loaded fires
   a `warning`, for example `Font "Brand Sans" 400 couldn't be loaded from Google Fonts (it isn't
   there, or the network failed)`. Google's 400 carries no CORS header, so in a browser a missing
   family and a network failure look the same. A `fetch` hook that answers 400 or 404 itself
   gets the more specific `isn't on Google Fonts`. Fallback fonts that fail produce one warning
   in total.
6. **Layout:** text lays out again as fonts arrive, as in the desktop app.

The viewer also contacts `registry.npmjs.org` for icon sets (Lucide, Feather, Phosphor), and
whatever hosts a document's image URLs point to. All of these need CORS, which Google and npm
send.

### Routing fonts yourself

Every download goes through `options.fetch(url)`, which defaults to `fetch`. Answer it from your
own server to proxy, cache, self-host, or add fonts Google doesn't have:

```js
const viewer = await createViewer(canvas, {
  fetch: async (url) => {
    const u = new URL(url);
    if (u.host === "fonts.googleapis.com") {
      // family is "Brand Sans:wght@700" or "Brand Sans:ital,wght@1,700"
      const [family, spec] = u.searchParams.get("family").split(":");
      if (family === "Brand Sans") {
        const weight = spec.split(/[@,]/).pop();
        const italic = spec.startsWith("ital") ? "-italic" : "";
        return new Response(`@font-face { src: url(https://cdn.example.com/brand-${weight}${italic}.woff2); }`);
      }
      return fetch(`/font-proxy/css2?${u.searchParams}`); // or straight to Google
    }
    if (u.host === "fonts.gstatic.com") return fetch(`/font-proxy/static${u.pathname}`);
    return fetch(url);
  },
});
```

- **Stylesheet format:** the response to a `css2` request can be any stylesheet in Google's
  format. Each `@font-face` contributes its `src: url(...)`, plus `unicode-range` if it has one.
  A rule without `unicode-range` is one complete font, and its glyphs are checked once it loads.
  That's what Google itself returns when a proxy calls it without a browser `User-Agent`: one
  complete TrueType file per weight. Fonts can be WOFF2, WOFF, TrueType or OpenType.
- **Missing fonts:** return status 400 or 404 for a family you don't have, and its text falls
  back as above.
- **Offline or locked-down deployments:** serve the stylesheets and files for every family your
  documents use, plus the fallback families, from your own origin through this hook. Google
  Fonts are open-licensed and can be self-hosted.

### Content Security Policy

If your app sends a CSP, it needs:

- `connect-src`: `fonts.googleapis.com`, `fonts.gstatic.com`, `registry.npmjs.org`, plus your
  image hosts, unless `fetch` routes through your origin.
- `script-src`: `'wasm-unsafe-eval'`, for the WebAssembly.
- `frame-src`: the origin serving `study-sandbox.html`.

If a sandbox loaded by URL doesn't start within 2 s (some embedded browsers block it), the
viewer loads the same page inline with `srcdoc`. Inline frames inherit the page's CSP, which
must then allow the sandbox's inline script, `worker-src blob: data:`, and `'unsafe-eval'` in
the worker.

## Not supported

- Editing, undo and Pen's panels: changes come from agents through the MCP tools.
- Pen's `pencil:` libraries.
- Fallback glyphs: these come from the Noto families, so characters a document's own font lacks
  can look slightly different from the desktop app, which uses the system's fonts.
