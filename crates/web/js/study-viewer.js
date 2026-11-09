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

/**
 * @typedef {object} NodeInfo
 * @property {string} id Node id, or `instanceId/childId` inside a component instance. This is
 *   what an agent passes to `Get`/`Update` in the `pencil` MCP tools.
 * @property {string|null} name
 * @property {string} type `frame`, `text`, `path`, `ref`, `script`, ...
 * @property {string|null} parent Id of the parent node (null for top-level frames)
 * @property {string|null} root Id of the top-level frame it's in
 * @property {string|null} instance Id of the component instance it's inside, if any
 * @property {{x:number,y:number,width:number,height:number}} bounds In document units
 * @property {{x:number,y:number,width:number,height:number}} screen In CSS pixels from the
 *   canvas's top-left corner: where to anchor a toolbar
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
 * @param {number} [options.scriptTimeout=5000] Milliseconds a `script` node may run.
 * @param {(file: string) => string} [options.locateFile] URL of study_web.wasm / study-sandbox.html.
 */
export async function createViewer(canvas, options = {}) {
  const gl = canvas.getContext("webgl2", { alpha: false, antialias: false, depth: false, stencil: true, premultipliedAlpha: true });
  if (!gl) throw new Error("study: this browser has no WebGL2");
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
 * - `warning` `{message}`: a script node failed, an import or file is missing, ...
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
  #sandbox = null;

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
    const read = this.#opts.readFile;
    if (read) {
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
        if (ticket !== this.#loads || this.#destroyed) return;
      }
    }
    const before = this.#selectionIds();
    const err = this.#withBytes(bytes, (p, n) => this.#withBytes(path, (pp, pn) => this.#m._study_load(this.#v, p, n, pp, pn)));
    this.redraw();
    if (err) throw new Error(this.#m.UTF8ToString(err));
    this.#emitIfSelectionChanged(before);
  }

  /** Provides or replaces a file at a path relative to your app's root (see `readFile`). */
  addFile(path, data) {
    this.#withBytes(path, (pp, pn) => this.#withBytes(toBytes(data), (p, n) => this.#m._study_add_file(this.#v, pp, pn, p, n)));
    this.redraw();
  }

  // ---------------------------------------------------------------- selection and nodes

  /** @returns {NodeInfo[]} */
  get selection() {
    return this.#selectionIds().map((id) => this.node(id)).filter(Boolean);
  }

  /** Selects nodes by id (an empty list clears the selection). */
  select(ids) {
    const before = this.#selectionIds();
    this.#withBytes(JSON.stringify(ids ?? []), (p, n) => this.#m._study_select(this.#v, p, n));
    this.redraw();
    this.#emitIfSelectionChanged(before);
  }

  /** @returns {NodeInfo|null} */
  node(id) {
    return this.#json(this.#withBytes(id, (p, n) => this.#m._study_node_info(this.#v, p, n)));
  }

  /** The node under a point in CSS pixels from the canvas's corner. @returns {NodeInfo|null} */
  nodeAt(x, y) {
    const id = this.#str(this.#m._study_hit(this.#v, x, y));
    return id ? this.node(id) : null;
  }

  /** The node's JSON as written in the .pen file (inside an instance: the component's node). */
  nodeJson(id) {
    return this.#json(this.#withBytes(id, (p, n) => this.#m._study_node_json(this.#v, p, n)));
  }

  /**
   * A PNG of the node as drawn (like the MCP screenshot tool: longest edge ≤ 1568 px, at most
   * 2× scale), for giving an agent visual context. @returns {Blob|null}
   */
  snapshot(id, { scale = 0 } = {}) {
    const ok = this.#withBytes(id, (p, n) => this.#m._study_snapshot(this.#v, p, n, scale));
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

  setTheme(theme) {
    this.#cleanup.find((f) => f.theme)?.();
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

  #str(ptr) { return ptr ? this.#m.UTF8ToString(ptr) : null; }
  #json(ptr) { return ptr ? JSON.parse(this.#m.UTF8ToString(ptr)) : null; }
  #selectionIds() { return this.#json(this.#m._study_selection(this.#v)) ?? []; }

  #emitIfSelectionChanged(before) {
    const after = this.#selectionIds();
    if (after.length === before.length && after.every((id, i) => id === before[i])) return;
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
      drag = { id: e.pointerId, x: e.clientX, y: e.clientY, startX: e.clientX, startY: e.clientY, panning: e.button === 1 };
      c.setPointerCapture(e.pointerId);
    });
    on(c, "pointermove", (e) => {
      const [x, y] = local(e);
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
        const id = this.#str(this.#m._study_hovered(this.#v));
        this.dispatchEvent(new CustomEvent("hover", { detail: { node: id ? this.node(id) : null } }));
      }
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
      const click = !drag.panning && e.type === "pointerup";
      drag = null;
      c.style.cursor = "";
      if (!click || !interactive()) return;
      const [x, y] = local(e);
      const node = this.nodeAt(x, y);
      const ev = new CustomEvent("nodeclick", { cancelable: true, detail: { node, x, y, event: e } });
      if (!this.dispatchEvent(ev)) return;
      const before = this.#selectionIds();
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
      const node = this.nodeAt(x, y);
      if (node && !this.#selectionIds().includes(node.id)) this.select([node.id]);
      this.dispatchEvent(new CustomEvent("nodecontextmenu", { detail: { node, x, y, event: e } }));
    });

    // The desktop app's keys, while the canvas has focus.
    if (!c.hasAttribute("tabindex")) c.tabIndex = 0;
    c.style.outline = "none";
    on(c, "keydown", (e) => {
      if (!interactive()) return;
      const mod = e.ctrlKey || e.metaKey;
      const before = this.#selectionIds();
      if (e.key === "Escape") this.select([]);
      else if (e.key === "Enter") {
        this.#m._study_select_relative(this.#v, e.shiftKey);
        this.redraw();
        this.#emitIfSelectionChanged(before);
      } else if (mod && e.key === "0") this.fit();
      else if (mod && e.key === "1") this.zoomBy(1 / this.zoom);
      else if (mod && e.key === "2") this.zoomTo(before);
      else return;
      e.preventDefault();
    });
    c.style.touchAction = "none";
  }
}
