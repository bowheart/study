// study's .pen canvas for web pages: the desktop app's Rust layout and Skia renderer, compiled to
// WebAssembly and drawn into a <canvas> with WebGL2.
//
//   import { createViewer } from "./study-viewer.js";
//   const viewer = await createViewer(canvas, { readFile });
//   await viewer.load(penJson, { path: "specs/home.pen" }); // again whenever the file changes
//   viewer.addEventListener("select", (e) => showToolbar(e.detail.nodes));
//   viewer.scan(["nodeId"]);                                  // agent presence
//
// study-web.js, study_web.wasm and study-sandbox.html must sit next to this file (or pass
// `locateFile`).

import createStudyModule from "./study-web.js";

const encoder = new TextEncoder();
const toBytes = (data) =>
  typeof data === "string" ? encoder.encode(data)
  : data instanceof Uint8Array ? data
  : data instanceof ArrayBuffer ? new Uint8Array(data)
  : ArrayBuffer.isView(data) ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
  : encoder.encode(JSON.stringify(data));
/** The exports' side argument: 1 for the changes layout's `before`, 0 for the shown document. */
const sideArg = (side) => (side === "before" ? 1 : 0);

/**
 * @typedef {object} NodeInfo
 * @property {string} id Node id, or `instanceId/childId` inside a component instance. This is
 *   what an agent passes to `Get`/`Update` in the `pencil` MCP tools.
 * @property {string|null} name
 * @property {string} type `frame`, `text`, `path`, `ref`, `script`, ...
 * @property {string|null} parent Id of the parent node (null for top-level frames)
 * @property {string|null} root Id of the top-level frame it's in
 * @property {string|null} instance Id of the component instance it's inside, if any
 * @property {"after"|"before"} side Which version it's in: `before` only for the older version in
 *   the changes layout (`showChanges`)
 * @property {{x:number,y:number,width:number,height:number}} bounds In document units
 * @property {{x:number,y:number,width:number,height:number}|null} screen In CSS pixels from the
 *   canvas's top-left corner: where to anchor a toolbar. Null in the changes layout when its
 *   frame isn't one of the rows.
 */

/**
 * @param {HTMLCanvasElement} canvas Sized with CSS; the viewer manages its pixel size.
 * @param {object} [options]
 * @param {"light"|"dark"|"system"} [options.theme="system"]
 * @param {(path: string) => Promise<Uint8Array|ArrayBuffer|string|null>} [options.readFile]
 *   Supplies files the document refers to by relative path: images, shaders, `script` node
 *   sources, imported `.pen` files. Paths are resolved against the `path` given to `load`.
 *   Return null if there's no such file.
 * @param {(url: string) => Promise<Response>} [options.fetch] Downloads fonts, icon libraries
 *   and remote images (default: `fetch`). Use it to route through a proxy or add auth.
 * @param {string[]} [options.fallbackFonts] Google Fonts families for characters a document's
 *   fonts lack, in order (default: Noto families covering symbols, emoji and the major scripts).
 * @param {boolean} [options.interactive=true] Click to select, hover outlines, keyboard.
 * @param {{breadcrumb?: boolean, zoom?: boolean, theme?: boolean}} [options.controls] Built-in
 *   controls drawn on the canvas, all on by default: the selection's breadcrumb (top left), zoom
 *   buttons and a light/dark switch (bottom right). Turn any off to build your own with
 *   `breadcrumb`, `select`, `zoomBy`, `fit` and `setTheme`.
 * @param {number} [options.scriptTimeout=5000] Milliseconds a `script` node may run.
 * @param {(file: string) => string} [options.locateFile] URL of study_web.wasm / study-sandbox.html.
 */
export async function createViewer(canvas, options = {}) {
  const gl = canvas.getContext("webgl2", { alpha: false, antialias: false, depth: false, stencil: true, premultipliedAlpha: true });
  if (!gl) throw new Error("study: this browser has no WebGL2");
  // Skia asks for the GPU's name when this extension is listed, but it isn't enabled, so the
  // browser logs INVALID_ENUM. Skia doesn't need it; enabling it is deprecated in Firefox.
  const supported = gl.getSupportedExtensions.bind(gl);
  gl.getSupportedExtensions = () => supported()?.filter((e) => e !== "WEBGL_debug_renderer_info") ?? null;
  const locateFile = options.locateFile ?? ((file) => new URL(file, import.meta.url).href);
  const module = await createStudyModule({ locateFile });
  return new Viewer(canvas, gl, module, { ...options, locateFile });
}

/**
 * Events (`viewer.addEventListener(type, (e) => ...)`, data in `e.detail`):
 * - `select` `{nodes: NodeInfo[]}`: the selection changed (click, keyboard, `select()`, or a
 *   `load` that removed selected nodes).
 * - `nodeclick` `{node: NodeInfo|null, x, y, event}`: a click on the canvas (not a drag), with the
 *   node under it. Fires before the selection updates; call `e.preventDefault()` to keep the
 *   selection as it is.
 * - `nodecontextmenu` `{node: NodeInfo|null, x, y, event}`: right-click. The node becomes the
 *   selection first unless it's already selected. Call `e.detail.event.preventDefault()` to
 *   suppress the browser's menu.
 * - `hover` `{node: NodeInfo|null}`
 * - `viewchange` `{camera: {x, y, zoom}}`: the camera moved. Re-read `node(id).screen` to keep a
 *   toolbar attached.
 * - `warning` `{message}`: a script node failed, or an import, image, shader or font couldn't be loaded
 * - `themechange` `{theme}`: the built-in light/dark switch was clicked (`"light"` or `"dark"`).
 * - `layoutchange` `{rows}`: the changes layout's rows changed size, place or content (a `load`,
 *   fonts or scripts arriving, a resize, new options). Scrolling fires `viewchange` instead.
 */
export class Viewer extends EventTarget {
  #m;
  #v;
  #ctx;
  #canvas;
  #opts;
  #fetch;
  #raf = 0;
  #destroyed = false;
  #cleanup = [];
  #camera = "";
  #loads = 0;
  #path = "document.pen";
  #compares = Promise.resolve();
  #changes = null;
  #changeOptions = null;
  #changeLoads = 0;
  #rowsVersion = 0;
  #sandbox = null;
  #theme = "system";
  #controls = { breadcrumb: true, zoom: true, theme: true };

  constructor(canvas, gl, module, options) {
    super();
    this.#m = module;
    this.#canvas = canvas;
    this.#opts = { interactive: true, scriptTimeout: 5000, ...options };
    this.#fetch = options.fetch ?? ((url) => fetch(url));
    this.#ctx = module.GL.registerContext(gl, { majorVersion: 2 });
    module.GL.makeContextCurrent(this.#ctx);
    const [w, h, dpr] = this.#pixelSize();
    canvas.width = w;
    canvas.height = h;
    this.#v = module._study_new(w, h, dpr);
    this.setControls(options.controls ?? {});
    if (options.fallbackFonts) this.#withBytes(JSON.stringify(options.fallbackFonts), (p, n) => module._study_set_fallback_fonts(this.#v, p, n));
    this.setTheme(options.theme ?? "system");
    this.#bindInput();
    const resize = new ResizeObserver(() => this.redraw());
    resize.observe(canvas);
    this.#cleanup.push(() => resize.disconnect());
    this.redraw();
  }

  // ---------------------------------------------------------------- document

  /**
   * Shows a .pen document: JSON text, a parsed object, or the file's bytes. `path` is where your
   * app keeps it; relative references resolve from its folder and are fetched through
   * `options.readFile` first. Calls made while an earlier load waits for files supersede it.
   * Rejects if the document isn't valid.
   */
  async load(pen, { path = "document.pen" } = {}) {
    const bytes = toBytes(pen);
    const ticket = ++this.#loads;
    if (!(await this.#provideFiles(bytes, path, () => ticket === this.#loads))) return;
    const before = this.#selectionKey();
    const err = this.#withBytes(bytes, (p, n) => this.#withBytes(path, (pp, pn) => this.#m._study_load(this.#v, p, n, pp, pn)));
    this.redraw();
    if (err) throw new Error(this.#m.UTF8ToString(err));
    this.#path = path;
    this.#emitIfSelectionChanged(before);
  }

  /** Provides or replaces a file at a path relative to your app's root (see `readFile`). */
  addFile(path, data) {
    this.#withBytes(path, (pp, pn) => this.#withBytes(toBytes(data), (p, n) => this.#m._study_add_file(this.#v, pp, pn, p, n)));
    this.redraw();
  }

  // ---------------------------------------------------------------- comparing versions

  /**
   * Outlines and tints nodes, apart from selection and hover: for showing what changed between
   * versions. `kind` is `"added"` (green), `"changed"` (amber, the default), `"removed"` (red) or a
   * `#rrggbb` colour. Each kind keeps its own set, so call once per kind; calling again replaces
   * that kind's set. `highlight([])` clears every kind; `highlight([], { kind })` clears one.
   */
  highlight(ids, { kind } = {}) {
    const ok = this.#withBytes(kind ?? "", (kp, kn) => this.#withBytes(JSON.stringify(ids ?? []), (p, n) => this.#m._study_highlight(this.#v, kp, kn, p, n)));
    this.redraw();
    if (!ok) throw new Error(`study: unknown highlight kind "${kind}"`);
  }

  /**
   * @typedef {object} Comparison
   * @property {string[]} added Nodes only in this viewer's document (outermost of each new subtree)
   * @property {string[]} removed Nodes only in `before` (outermost of each removed subtree)
   * @property {{id: string, props: string[], moved: boolean,
   *   bounds: {before: object, after: object}|null}[]} changed Nodes in both that differ: properties
   *   whose values differ, whether it changed parent or order, and its laid-out box when that moved
   *   or resized (also when its own properties didn't change, for example pushed by a sibling)
   * @property {string[]} frames Top-level nodes containing any of these
   */

  /**
   * What changed from another version of the document (`before`) to the one this viewer shows,
   * as laid out and rendered. Nodes match by id, or by instance path inside component instances,
   * which are compared as they render, so a component edit shows up on every instance. Waits up to
   * `timeout` ms for fonts and `script` nodes, so text measures the same in both. `path` is where
   * `before` is kept, for its relative files (default: the same as the shown document).
   * @returns {Promise<Comparison>}
   */
  compare(before, { path = this.#path, timeout = 10000 } = {}) {
    const run = async () => {
      const bytes = toBytes(before);
      await this.#provideFiles(bytes, path, () => true);
      if (this.#destroyed) throw new Error("study: viewer destroyed");
      const err = this.#withBytes(bytes, (p, n) => this.#withBytes(path, (pp, pn) => this.#m._study_compare_start(this.#v, p, n, pp, pn)));
      if (err) throw new Error(this.#m.UTF8ToString(err));
      await this.#settle(timeout);
      return this.#json(this.#m._study_compare_result(this.#v));
    };
    const result = this.#compares.then(run, run);
    this.#compares = result.catch(() => {});
    return result;
  }

  /**
   * Draws another document over the shown one, translucent (onion skin). `null` removes it.
   * It isn't interactive; selection and hover stay on the shown document.
   */
  async setOverlay(pen, { path = this.#path, opacity = 0.5 } = {}) {
    if (pen == null) {
      this.#withBytes("", (p, n) => this.#m._study_set_overlay(this.#v, p, n, p, n, 0));
    } else {
      const bytes = toBytes(pen);
      await this.#provideFiles(bytes, path, () => true);
      if (this.#destroyed) return;
      const err = this.#withBytes(bytes, (p, n) => this.#withBytes(path, (pp, pn) => this.#m._study_set_overlay(this.#v, p, n, pp, pn, opacity)));
      if (err) throw new Error(this.#m.UTF8ToString(err));
    }
    this.redraw();
  }

  /** The overlay's opacity, 0–1 (also the changes layout's onion opacity). */
  set overlayOpacity(opacity) {
    this.#m._study_set_overlay_opacity(this.#v, opacity);
    if (this.#changeOptions) this.#changeOptions.opacity = opacity;
    this.redraw();
  }

  /**
   * @typedef {object} ChangeRow
   * @property {string} id The top-level node's id
   * @property {string|null} name
   * @property {"added"|"changed"|"removed"|"unchanged"} kind `unchanged` only for frames you
   *   asked for in `frames` that didn't change
   * @property {string[]} props The frame's own properties that differ
   * @property {boolean} moved The frame changed order among the top-level nodes
   * @property {{x:number,y:number,width:number,height:number}|null} before Where the older
   *   version is drawn, in CSS px from the canvas's top-left (null if it's new)
   * @property {{x:number,y:number,width:number,height:number}|null} after Where the shown version
   *   is drawn (null if it was removed)
   * @property {{x:number,y:number,width:number,height:number}} bounds The whole row, both sides
   */

  /**
   * Shows only the top-level frames that differ from another version, one row each, laid out by
   * the viewer in one canvas: `before` on the left and the shown document on the right, at the same
   * scale (`mode: "onion"`: one column, `before` drawn over the shown version at `opacity`). Both
   * versions load fully and once, so variables, components, imports, fonts and scripts resolve
   * as usual, and ids are the documents' own. Changed and added nodes are outlined on the right,
   * removed ones on the left. Scrolling and zooming move all rows together. Hover and selection
   * work on both sides (`NodeInfo.side` says which); in the onion mode, on the shown version only.
   *
   * It stays until `showChanges(null)`; `load` of a newer version recomputes the rows, and so do
   * fonts and scripts arriving (`layoutchange`). Resolves once fonts and scripts have settled (or
   * after `timeout` ms).
   *
   * @param {object} [options]
   * @param {string} [options.path] Where `before` is kept, for its relative files (default: the
   *   shown document's)
   * @param {string[]} [options.frames] Top-level ids to show (default: every frame `compare`
   *   reports), in document order either way
   * @param {"side"|"onion"} [options.mode="side"]
   * @param {number} [options.opacity=0.5] The onion mode's `before` opacity
   * @param {"width"|"contain"} [options.fit="width"] `width`: each row fills the column width;
   *   `contain`: also no taller than `maxRowHeight` (default: the canvas height)
   * @param {number} [options.maxRowHeight] Rows taller than this (CSS px) are scaled down
   * @param {number} [options.gap=32] Between rows (room for your row headers)
   * @param {number} [options.gutter=24] Between the two sides
   * @param {number} [options.padding=24] Around the rows
   * @param {{added?: string, changed?: string, removed?: string}} [options.colors] `#rrggbb`
   * @returns {Promise<ChangesView|null>}
   */
  async showChanges(before, options = {}) {
    const ticket = ++this.#changeLoads;
    if (before == null) {
      this.#m._study_hide_changes(this.#v);
      this.#changes = null;
      this.#changeOptions = null;
      this.#rowsVersion = 0;
      this.#emitIfSelectionChanged(this.#selectionKey());
      this.redraw();
      return null;
    }
    const { path = this.#path, timeout = 10000, ...opts } = options;
    const bytes = toBytes(before);
    if (!(await this.#provideFiles(bytes, path, () => ticket === this.#changeLoads)) || this.#destroyed) return this.#changes;
    const selection = this.#selectionKey();
    const err = this.#withBytes(bytes, (p, n) => this.#withBytes(path, (pp, pn) => this.#withBytes(JSON.stringify(opts), (op, on) =>
      this.#m._study_show_changes(this.#v, p, n, pp, pn, op, on))));
    if (err) throw new Error(this.#m.UTF8ToString(err));
    this.#changeOptions = { ...opts };
    this.#changes ??= new ChangesView({
      rows: () => this.#json(this.#m._study_changes_rows(this.#v)) ?? [],
      rowAt: (x, y) => this.#json(this.#m._study_row_at(this.#v, x, y)),
      scrollToRow: (id) => {
        const ok = !!this.#withBytes(id, (p, n) => this.#m._study_scroll_to_row(this.#v, p, n));
        this.redraw();
        return ok;
      },
      options: () => ({ ...this.#changeOptions }),
      setOptions: (more) => {
        const next = { ...this.#changeOptions, ...more };
        const err = this.#withBytes(JSON.stringify(next), (p, n) => this.#m._study_changes_options(this.#v, p, n));
        if (err) throw new Error(this.#m.UTF8ToString(err));
        const before = this.#selectionKey();
        this.#changeOptions = next;
        this.redraw();
        this.#emitIfSelectionChanged(before);
      },
      flash: (ms) => {
        this.#m._study_flash_highlights(this.#v, ms);
        this.redraw();
      },
      close: () => this.showChanges(null),
      active: (view) => this.#changes === view,
    });
    this.#emitIfSelectionChanged(selection);
    this.redraw();
    await this.#settle(timeout);
    return this.#changes;
  }

  /** The changes layout, while it's shown. @returns {ChangesView|null} */
  get changes() { return this.#changes; }

  // ---------------------------------------------------------------- selection and nodes

  /** @returns {NodeInfo[]} */
  get selection() {
    const { ids, side } = this.#selectionState();
    return ids.map((id) => this.node(id, { side })).filter(Boolean);
  }

  /**
   * Selects nodes by id (an empty list clears the selection). In the changes layout, `side:
   * "before"` selects in the older version.
   */
  select(ids, { side = "after" } = {}) {
    const before = this.#selectionKey();
    this.#withBytes(JSON.stringify(ids ?? []), (p, n) => this.#m._study_select(this.#v, p, n, sideArg(side)));
    this.redraw();
    this.#emitIfSelectionChanged(before);
  }

  /** `side: "before"`: the node in the changes layout's older version. @returns {NodeInfo|null} */
  node(id, { side = "after" } = {}) {
    return this.#json(this.#withBytes(id, (p, n) => this.#m._study_node_info(this.#v, p, n, sideArg(side))));
  }

  /** The node under a point in CSS pixels from the canvas's corner. @returns {NodeInfo|null} */
  nodeAt(x, y) {
    const hit = this.#json(this.#m._study_hit(this.#v, x, y));
    return hit ? this.node(hit.id, hit) : null;
  }

  /** The node's JSON as written in the .pen file (inside an instance: the component's node). */
  nodeJson(id, { side = "after" } = {}) {
    return this.#json(this.#withBytes(id, (p, n) => this.#m._study_node_json(this.#v, p, n, sideArg(side))));
  }

  /**
   * A PNG of the node as drawn (like the MCP screenshot tool: longest edge ≤ 1568 px, at most
   * 2× scale), for giving an agent visual context. @returns {Blob|null}
   */
  snapshot(id, { scale = 0, side = "after" } = {}) {
    const ok = this.#withBytes(id, (p, n) => this.#m._study_snapshot(this.#v, p, n, scale, sideArg(side)));
    if (!ok) return null;
    const ptr = this.#m._study_blob_ptr(this.#v);
    const len = this.#m._study_blob_len(this.#v);
    return new Blob([this.#m.HEAPU8.slice(ptr, ptr + len)], { type: "image/png" });
  }

  // ---------------------------------------------------------------- camera

  /** Fits the given nodes in view, or everything. */
  zoomTo(ids = []) {
    this.#withBytes(JSON.stringify(ids), (p, n) => this.#m._study_fit(this.#v, p, n));
    this.redraw();
  }
  fit() { this.zoomTo([]); }
  zoomBy(factor, x = this.#canvas.clientWidth / 2, y = this.#canvas.clientHeight / 2) {
    this.#m._study_zoom(this.#v, factor, x, y);
    this.redraw();
  }
  /** `{x, y, zoom}`: the document point at the canvas's top-left corner, and CSS pixels per unit. */
  get camera() { return this.#json(this.#m._study_camera(this.#v)); }
  set camera({ x, y, zoom }) {
    this.#m._study_set_camera(this.#v, x, y, zoom);
    this.redraw();
  }
  get zoom() { return this.camera.zoom; }

  /** `"light"`, `"dark"` or `"system"` (follows the OS). */
  get theme() { return this.#theme; }
  setTheme(theme) {
    this.#theme = theme;
    this.#cleanup.find((f) => f.theme)?.();
    const i = this.#cleanup.findIndex((f) => f.theme);
    if (i >= 0) this.#cleanup.splice(i, 1);
    const media = matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      this.#m._study_set_dark(this.#v, theme === "dark" || (theme === "system" && media.matches));
      this.redraw();
    };
    if (theme === "system") {
      media.addEventListener("change", apply);
      const off = () => media.removeEventListener("change", apply);
      off.theme = true;
      this.#cleanup.push(off);
    }
    apply();
  }

  /** Shows or hides built-in controls: `{breadcrumb, zoom, theme}`, any subset. */
  setControls(controls) {
    Object.assign(this.#controls, controls);
    const c = this.#controls;
    this.#m._study_set_controls(this.#v, !!c.breadcrumb, !!c.zoom, !!c.theme);
    this.redraw();
  }

  /**
   * The last selected node and its ancestors, top-level frame first: what the built-in
   * breadcrumb shows. A click on the built-in one replaces only the last selected node, keeping
   * the rest of the selection. @returns {NodeInfo[]}
   */
  get breadcrumb() {
    return (this.#json(this.#m._study_breadcrumb(this.#v)) ?? []).map((id) => this.node(id)).filter(Boolean);
  }

  // ---------------------------------------------------------------- agent presence

  /** Nodes an agent read get a scan sweep. */
  scan(ids) { this.#presence("_study_scan", ids); }
  /** Top-level frames an agent edited glow ("Claude is working") for ~20 s. */
  working(ids) { this.#presence("_study_touch", ids); }
  /** Paths an agent created are drawn in by the calligraphy pen. */
  drawIn(ids) { this.#presence("_study_draw_in", ids); }

  // ---------------------------------------------------------------- lifecycle

  redraw() {
    if (this.#raf || this.#destroyed) return;
    this.#raf = requestAnimationFrame(() => {
      this.#raf = 0;
      if (this.#destroyed) return;
      this.#m.GL.makeContextCurrent(this.#ctx);
      const [w, h, dpr] = this.#pixelSize();
      if (w !== this.#canvas.width || h !== this.#canvas.height) {
        this.#canvas.width = w;
        this.#canvas.height = h;
        this.#m._study_resize(this.#v, w, h, dpr);
      }
      const more = this.#m._study_frame(this.#v);
      this.#afterFrame();
      if (more) this.redraw();
    });
  }

  destroy() {
    this.#destroyed = true;
    cancelAnimationFrame(this.#raf);
    this.#cleanup.forEach((f) => f());
    this.#sandbox?.frame.remove();
    this.#m._study_destroy(this.#v);
    this.#m.GL.deleteContext(this.#ctx);
  }

  // ---------------------------------------------------------------- internals

  #pixelSize() {
    const dpr = window.devicePixelRatio || 1;
    const w = Math.max(1, Math.round(this.#canvas.clientWidth * dpr));
    const h = Math.max(1, Math.round(this.#canvas.clientHeight * dpr));
    return [w, h, w / Math.max(1, this.#canvas.clientWidth)];
  }

  /** Copies a string or bytes into wasm memory for the duration of `f(ptr, len)`. */
  #withBytes(data, f) {
    const bytes = typeof data === "string" ? encoder.encode(data) : data;
    const p = this.#m._study_alloc(bytes.length);
    this.#m.HEAPU8.set(bytes, p);
    try {
      return f(p, bytes.length);
    } finally {
      this.#m._study_dealloc(p, bytes.length);
    }
  }

  /**
   * Asks `options.readFile` for the files a document refers to that the viewer doesn't have,
   * including those of imported documents. False if `wanted()` says the caller gave up meanwhile.
   */
  async #provideFiles(bytes, path, wanted) {
    const read = this.#opts.readFile;
    if (!read) return true;
    const tried = new Set();
    for (let round = 0; round < 8; round++) {
      const missing = this.#json(this.#withBytes(bytes, (p, n) => this.#withBytes(path, (pp, pn) => this.#m._study_file_refs(this.#v, p, n, pp, pn)))) ?? [];
      const todo = missing.filter((f) => !tried.has(f));
      if (!todo.length) break;
      await Promise.all(todo.map(async (file) => {
        tried.add(file);
        let data = null;
        try { data = await read(file); } catch { data = null; }
        if (data != null && !this.#destroyed) this.addFile(file, data);
      }));
      if (!wanted() || this.#destroyed) return false;
    }
    return true;
  }

  /** Waits up to `timeout` ms for fonts and script runs that could change a layout. */
  async #settle(timeout) {
    const until = performance.now() + timeout;
    while (this.#m._study_settling(this.#v) && performance.now() < until && !this.#destroyed) {
      // Not through frames: a background tab gets none.
      this.#m._study_pump(this.#v);
      this.#startDownloads();
      this.#startScripts();
      await new Promise((r) => setTimeout(r, 30));
    }
    if (!this.#destroyed) this.#m._study_pump(this.#v);
  }

  #str(ptr) { return ptr ? this.#m.UTF8ToString(ptr) : null; }
  #json(ptr) { return ptr ? JSON.parse(this.#m.UTF8ToString(ptr)) : null; }
  /** `{ids, side}`: the selection is on one side of the changes layout at a time. */
  #selectionState() { return this.#json(this.#m._study_selection(this.#v)) ?? { ids: [], side: "after" }; }
  #selectionIds() { return this.#selectionState().ids; }
  #selectionKey() { return this.#str(this.#m._study_selection(this.#v)); }

  #emitIfSelectionChanged(before) {
    if (this.#selectionKey() === before) return;
    this.dispatchEvent(new CustomEvent("select", { detail: { nodes: this.selection } }));
  }

  #presence(fn, ids) {
    if (!ids?.length) return;
    this.#withBytes(JSON.stringify(ids), (p, n) => this.#m[fn](this.#v, p, n));
    this.redraw();
  }

  #afterFrame() {
    this.#startDownloads();
    this.#startScripts();
    for (const message of this.#json(this.#m._study_warnings(this.#v)) ?? []) {
      console.warn(`study: ${message}`);
      this.dispatchEvent(new CustomEvent("warning", { detail: { message } }));
    }
    const camera = this.#str(this.#m._study_camera(this.#v));
    if (camera !== this.#camera) {
      this.#camera = camera;
      this.dispatchEvent(new CustomEvent("viewchange", { detail: { camera: JSON.parse(camera) } }));
    }
    const version = this.#m._study_changes_version(this.#v);
    if (this.#changes && version !== this.#rowsVersion) {
      this.#rowsVersion = version;
      this.dispatchEvent(new CustomEvent("layoutchange", { detail: { rows: this.#changes.rows } }));
    }
  }

  #startDownloads() {
    for (const [id, url] of this.#json(this.#m._study_requests(this.#v)) ?? []) {
      this.#download(id, url);
    }
  }

  async #download(id, url) {
    let status = 0;
    let body = new Uint8Array(0);
    try {
      const res = await this.#fetch(url);
      status = res.status;
      if (res.ok) body = new Uint8Array(await res.arrayBuffer());
    } catch {
      status = 0;
    }
    if (this.#destroyed) return;
    this.#withBytes(body, (p, n) => this.#m._study_fulfill(id, status, p, n));
    this.#startDownloads(); // a reply can lead to a follow-up request (font CSS → font file)
    this.redraw();
  }

  /** `script` nodes run in study-sandbox.html: an opaque-origin iframe with a killable worker. */
  #startScripts() {
    const jobs = this.#json(this.#m._study_script_jobs(this.#v));
    if (!jobs) return;
    if (!this.#sandbox) {
      const url = this.#opts.locateFile("study-sandbox.html");
      const makeFrame = () => {
        const frame = document.createElement("iframe");
        frame.setAttribute("sandbox", "allow-scripts");
        frame.setAttribute("aria-hidden", "true");
        frame.style.cssText = "position:absolute;width:0;height:0;border:0;visibility:hidden";
        return frame;
      };
      const sandbox = { frame: makeFrame(), ready: false, waiting: [] };
      sandbox.frame.src = url;
      // Some embedded browsers don't run scripts in a sandboxed frame loaded by URL; the same
      // page inline (srcdoc) still gets an opaque origin. srcdoc inherits the page's CSP, though.
      const fallback = setTimeout(async () => {
        if (sandbox.ready || this.#destroyed) return;
        const html = await (await fetch(url)).text();
        const frame = makeFrame();
        frame.srcdoc = html;
        sandbox.frame.replaceWith(frame);
        sandbox.frame = frame;
      }, 2000);
      this.#cleanup.push(() => clearTimeout(fallback));
      const onMessage = (e) => {
        const frame = sandbox.frame;
        if (e.source !== frame.contentWindow) return;
        if (e.data?.studyScriptReady) {
          sandbox.ready = true;
          sandbox.waiting.splice(0).forEach((job) => frame.contentWindow.postMessage({ studyScript: job }, "*"));
        } else if (e.data?.studyScript && !this.#destroyed) {
          const { id, ok, text } = e.data.studyScript;
          this.#withBytes(String(id), (kp, kn) => this.#withBytes(String(text ?? ""), (p, n) => this.#m._study_script_result(this.#v, kp, kn, !!ok, p, n)));
          this.redraw();
        }
      };
      window.addEventListener("message", onMessage);
      this.#cleanup.push(() => window.removeEventListener("message", onMessage));
      document.body.appendChild(sandbox.frame);
      this.#sandbox = sandbox;
    }
    for (const [id, code] of jobs) {
      const job = { id, code, timeout: this.#opts.scriptTimeout };
      if (this.#sandbox.ready) this.#sandbox.frame.contentWindow.postMessage({ studyScript: job }, "*");
      else this.#sandbox.waiting.push(job);
    }
  }

  /** A click on a built-in control: the viewer acts, then the page hears about it. */
  #pressControl(x, y) {
    const before = this.#selectionKey();
    const what = this.#json(this.#m._study_press(this.#v, x, y));
    this.redraw();
    if (what?.control === "breadcrumb") this.#emitIfSelectionChanged(before);
    if (what?.control === "theme") {
      const theme = what.dark ? "dark" : "light";
      this.setTheme(theme);
      this.dispatchEvent(new CustomEvent("themechange", { detail: { theme } }));
    }
  }

  #bindInput() {
    const c = this.#canvas;
    const on = (target, type, f, opts) => {
      target.addEventListener(type, f, opts);
      this.#cleanup.push(() => target.removeEventListener(type, f, opts));
    };
    const local = (e) => {
      const r = c.getBoundingClientRect();
      return [e.clientX - r.left, e.clientY - r.top];
    };
    const interactive = () => this.#opts.interactive;

    // Scroll pans; Ctrl/Cmd+scroll and trackpad pinch (which browsers send as Ctrl+wheel) zoom.
    on(c, "wheel", (e) => {
      e.preventDefault();
      const unit = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? c.clientHeight : 1;
      const [x, y] = local(e);
      if (e.ctrlKey || e.metaKey) {
        const dy = e.deltaY * unit;
        // Small pixel deltas are trackpad pinches (scale = e^(-dy/100)); notches step like the app.
        const factor = e.deltaMode === 0 && Math.abs(dy) < 50 ? Math.exp(-dy / 100) : Math.pow(1.0018, -dy / 2);
        this.#m._study_zoom(this.#v, factor, x, y);
      } else {
        this.#m._study_pan(this.#v, -e.deltaX * unit, -e.deltaY * unit);
      }
      this.redraw();
    }, { passive: false });
    // Safari reports trackpad pinches as gesture events instead.
    let gestureScale = 1;
    on(c, "gesturestart", (e) => { e.preventDefault(); gestureScale = 1; });
    on(c, "gesturechange", (e) => {
      e.preventDefault();
      const [x, y] = local(e);
      this.#m._study_zoom(this.#v, e.scale / gestureScale, x, y);
      gestureScale = e.scale;
      this.redraw();
    });

    // Left button: click selects, drag pans. Middle button always pans.
    let drag = null;
    on(c, "pointerdown", (e) => {
      if (e.button !== 0 && e.button !== 1) return;
      e.preventDefault();
      c.focus({ preventScroll: true });
      const [x, y] = local(e);
      if (e.button === 0 && this.#m._study_on_controls(this.#v, x, y)) {
        drag = { id: e.pointerId, control: true };
        c.setPointerCapture(e.pointerId);
        return;
      }
      drag = { id: e.pointerId, x: e.clientX, y: e.clientY, startX: e.clientX, startY: e.clientY, panning: e.button === 1 };
      c.setPointerCapture(e.pointerId);
    });
    on(c, "pointermove", (e) => {
      const [x, y] = local(e);
      if (drag?.id === e.pointerId && drag.control) return;
      if (drag?.id === e.pointerId) {
        if (!drag.panning && Math.hypot(e.clientX - drag.startX, e.clientY - drag.startY) > 4) drag.panning = true;
        if (drag.panning) {
          c.style.cursor = "grabbing";
          this.#m._study_pan(this.#v, e.clientX - drag.x, e.clientY - drag.y);
          this.redraw();
        }
        drag.x = e.clientX;
        drag.y = e.clientY;
        return;
      }
      if (this.#m._study_pointer(this.#v, x, y, interactive())) {
        const h = this.#json(this.#m._study_hovered(this.#v));
        this.dispatchEvent(new CustomEvent("hover", { detail: { node: h ? this.node(h.id, h) : null } }));
      }
      c.style.cursor = this.#m._study_over_control(this.#v) ? "pointer" : "";
      this.redraw();
    });
    on(c, "pointerleave", () => {
      if (this.#m._study_pointer_left(this.#v)) {
        this.dispatchEvent(new CustomEvent("hover", { detail: { node: null } }));
        this.redraw();
      }
    });
    const end = (e) => {
      if (drag?.id !== e.pointerId) return;
      if (drag.control) {
        drag = null;
        if (e.type === "pointerup") this.#pressControl(...local(e));
        return;
      }
      const click = !drag.panning && e.type === "pointerup";
      drag = null;
      c.style.cursor = "";
      if (!click || !interactive()) return;
      const [x, y] = local(e);
      const node = this.nodeAt(x, y);
      const ev = new CustomEvent("nodeclick", { cancelable: true, detail: { node, x, y, event: e } });
      if (!this.dispatchEvent(ev)) return;
      const before = this.#selectionKey();
      this.#m._study_click(this.#v, x, y, e.shiftKey);
      this.redraw();
      this.#emitIfSelectionChanged(before);
    };
    on(c, "pointerup", end);
    on(c, "pointercancel", end);
    on(c, "auxclick", (e) => e.button === 1 && e.preventDefault());
    on(c, "contextmenu", (e) => {
      if (!interactive()) return;
      const [x, y] = local(e);
      if (this.#m._study_on_controls(this.#v, x, y)) return;
      const node = this.nodeAt(x, y);
      const sel = this.#selectionState();
      if (node && !(sel.side === node.side && sel.ids.includes(node.id))) this.select([node.id], node);
      this.dispatchEvent(new CustomEvent("nodecontextmenu", { detail: { node, x, y, event: e } }));
    });

    // The desktop app's keys, while the canvas has focus.
    if (!c.hasAttribute("tabindex")) c.tabIndex = 0;
    c.style.outline = "none";
    on(c, "keydown", (e) => {
      if (!interactive()) return;
      const mod = e.ctrlKey || e.metaKey;
      const before = this.#selectionKey();
      if (e.key === "Escape") this.select([]);
      else if (e.key === "Enter") {
        this.#m._study_select_relative(this.#v, e.shiftKey);
        this.redraw();
        this.#emitIfSelectionChanged(before);
      } else if (mod && e.key === "0") this.fit();
      else if (mod && e.key === "1") this.zoomBy(1 / this.zoom);
      else if (mod && e.key === "2") this.zoomTo(this.#selectionIds());
      else return;
      e.preventDefault();
    });
    c.style.touchAction = "none";
  }
}

/**
 * The changes layout of a viewer (`viewer.showChanges`). Row geometry is in CSS px from the
 * canvas's top-left and moves with the camera: re-read `rows` on `viewchange` and
 * `layoutchange` to keep your row headers in place.
 */
export class ChangesView {
  #api;

  /** @hideconstructor */
  constructor(api) {
    this.#api = api;
  }

  /** @returns {ChangeRow[]} In document order. */
  get rows() { return this.#active() ? this.#api.rows() : []; }

  /**
   * The row at a point in CSS px, with `side` (`"before"`/`"after"`; null in the onion mode).
   * Includes the gap above and below it. @returns {(ChangeRow & {side: string|null})|null}
   */
  rowAt(x, y) { return this.#active() ? this.#api.rowAt(x, y) : null; }

  /** Scrolls a row to the top. False if there's no row for that id. */
  scrollToRow(id) { return this.#active() && this.#api.scrollToRow(id); }

  /** `"side"` or `"onion"`. */
  get mode() { return this.options.mode ?? "side"; }

  /**
   * The options in effect (as passed to `showChanges`, with later `setOptions`). `highlights` is
   * always present.
   */
  get options() { return this.#active() ? { highlights: true, ...this.#api.options() } : {}; }

  /**
   * Changes options without loading `before` again: `mode`, `opacity`, `fit`, `maxRowHeight`,
   * `gap`, `gutter`, `padding`, `frames`, `colors`, `highlights`. Missing ones keep their values.
   */
  setOptions(options) {
    if (this.#active()) this.#api.setOptions(options);
  }

  /**
   * Shows the added/changed/removed highlights for `duration` ms, then fades them out, while
   * `highlights` is off. Does nothing visible while they're on.
   */
  flashHighlights({ duration = 1000 } = {}) {
    if (this.#active()) this.#api.flash(duration);
  }

  /** Back to the whole document (`viewer.showChanges(null)`). */
  close() { return this.#active() ? this.#api.close() : Promise.resolve(null); }

  #active() { return this.#api.active(this); }
}
