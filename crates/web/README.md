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

Copy these four files from `crates/web/dist/` into one folder in your app's static files:

| File | What it is |
| --- | --- |
| `study-viewer.js` | The API (an ES module) |
| `study-web.js` | Emscripten's loader, imported by `study-viewer.js` |
| `study_web.wasm` | The renderer, about 3.6 MB gzipped |
| `study-sandbox.html` | The page `script` nodes run in |

Bundlers that understand `new URL(..., import.meta.url)`, such as Vite, find the files on their
own. Otherwise pass `locateFile: (name) => "/static/study/" + name`. `dist/demo.html` shows every
feature; serve `dist/` and open it.

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

## Selection, clicks and a toolbar

Every node is described by a `NodeInfo`:

```ts
{
  id: string;          // node id, or "instanceId/childId" inside a component instance
  name: string | null;
  type: string;        // "frame", "text", "ref", "script", ...
  parent: string | null;
  root: string | null; // the top-level frame it's in
  instance: string | null; // the component instance it's inside, if any
  bounds: { x, y, width, height }; // document units
  screen: { x, y, width, height }; // CSS pixels from the canvas's top-left corner
}
```

| Member | |
| --- | --- |
| `viewer.selection` | `NodeInfo[]` |
| `viewer.select(ids)` | Replace the selection (`[]` clears it) |
| `viewer.node(id)` / `viewer.nodeAt(x, y)` | `NodeInfo` or `null` |
| `viewer.nodeJson(id)` | The node as written in the `.pen` file |
| `viewer.snapshot(id, { scale })` | A PNG `Blob` of the node as drawn |
| `viewer.zoomTo(ids)`, `fit()`, `zoomBy(f)`, `camera` | View control |

Events come from `viewer.addEventListener(type, (e) => …)`, with data in `e.detail`:

| Event | Detail | When |
| --- | --- | --- |
| `select` | `{ nodes }` | The selection changed |
| `nodeclick` | `{ node, x, y, event }` | A click, not a drag. Fires before selecting; `e.preventDefault()` keeps the selection |
| `nodecontextmenu` | `{ node, x, y, event }` | Right-click. The node is selected first. `e.detail.event.preventDefault()` hides the browser menu |
| `hover` | `{ node }` | The node under the pointer changed |
| `viewchange` | `{ camera }` | The view moved; re-read `node(id).screen` |
| `warning` | `{ message }` | A script failed, a file is missing, ... |

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

1. **In the message.** Send the selection with the user's question, for example:

   ```js
   const [node] = viewer.selection;
   const context = {
     file: "/srv/specs/home.pen",       // the path the agent's MCP server opens
     node,                              // id, name, type, place in the tree
     json: viewer.nodeJson(node.id),    // what the node contains
   };
   const image = viewer.snapshot(node.id); // optional: what it looks like (PNG Blob)
   ```

2. **As the app's state.** When the agents' MCP server runs with
   `STUDY_SELECTION=/path/view.json`, keep that file up to date:

   ```json
   {"file": "/srv/specs/home.pen", "selection": ["ZcurH"]}
   ```

   `get_app_state`, the first call an agent makes, then reports that document and selection the
   same way the desktop app reports its own. Use one file per agent session.

## Showing an agent's work live

Run the agents' MCP server headless, logging what each call did:

```bash
STUDY_HEADLESS=1 STUDY_EVENTS=/srv/specs/events.jsonl STUDY_SELECTION=/srv/specs/view.json study mcp
```

Each `execute` call saves the `.pen` file and appends a line to the events file:

```json
{"file": "/srv/specs/home.pen", "time": 1794241048796, "read": ["ZcurH"], "edited": ["ZcurH"], "drawn": ["HYOgq"]}
```

Push the saved file and the event to the page, over your WebSocket or server-sent events. Load
the file first, then replay the event:

```js
await viewer.load(newPenText, { path: "specs/home.pen" });
viewer.working(event.edited); // top-level frames glow ("Claude is working") for ~20 s
viewer.drawIn(event.drawn);   // new paths are drawn in by the pen
viewer.scan(event.read);      // nodes it read get a scan sweep
```

`load` takes 1 ms for a typical file, 0.7 s for 230K nodes and 3.3 s for 920K.

## Files, imports and scripts

A document can refer to files by relative path: image fills, shader sources, `script` node
sources and `imports` of other `.pen` files. Before `load` lays a document out, it asks
`options.readFile(path)` for each file it doesn't have yet. Paths are relative to your app's root
and resolved from the document's `path`. Return a `Uint8Array`, `ArrayBuffer` or string, or
`null` if the file doesn't exist. Imported documents are scanned for their own files too.

`viewer.addFile(path, data)` provides or replaces a file directly, for example after the agent
regenerates an image. Without `readFile`, files can only arrive through `addFile`, and missing
ones produce `warning` events.

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
   uses, from `fonts.gstatic.com`. Latin always loads, and WOFF2 is decoded in the viewer. A
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
5. **Layout:** text lays out again as fonts arrive, as in the desktop app.

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
  complete TrueType file per weight. Fonts can be WOFF2, TrueType or OpenType.
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
