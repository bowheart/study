# The `browser` tool

`browser` drives a real Chrome/Chromium through the DevTools Protocol, so you can build from a
live page: import it as editable layers, check your own render against it, or read its DOM and
computed styles. It works headless on a server — no app window is needed.

```ts
browser({
  action: "load-page" | "import-to-canvas" | "screenshot-to-canvas" | "return-element" | "return-screenshot" | "evaluate" | "cdp" | "close",
  filePath?: string,        // the .pen document results belong to; assets are saved beside it
  nodeId?: string,          // insert into this frame instead of free space on the page
  target?: "full-page" | "selection" | "query",   // default full-page
  querySelector?: string,   // with target "query"
  url?: string,             // load-page: http(s), file or data URL
  viewport?: {width, height},
  waitFor?: string,         // load-page: wait for this selector (SPAs)
  timeout?: number,         // ms for a load or selector, default 30000
  method?: string,          // cdp
  params?: object,          // cdp; evaluate uses {expression}
})
```

## Actions

| Action | What it does |
| --- | --- |
| `load-page` | Navigates and waits for the document. Returns title, final URL and sizes. |
| `import-to-canvas` | Reproduces the target as pen layers (see below). Returns the new frame's id. |
| `screenshot-to-canvas` | Places a PNG of the target on the canvas as one image layer. |
| `return-element` | Returns the target's tag, classes, rect, text, outer HTML and computed styles as JSON text. |
| `return-screenshot` | Returns a PNG of the target as an image you can look at. |
| `evaluate` | Runs `params.expression` in the page and returns its JSON value (study extension). |
| `cdp` | Sends one CDP command and returns the result; `Page.captureScreenshot` comes back as an image. |
| `close` | Stops the browser and frees its process (study extension). |

`target: "selection"` needs a visible browser with an element picker; a headless browser does
not have one, so use `target: "query"` with a selector.

## How the import maps

- **Layout.** A container whose children form one clean run — ordered, non-overlapping, equal
  gaps, same cross-axis start — becomes a `horizontal` or `vertical` frame with the measured
  `gap` and `padding`. Everything else keeps absolute `x`/`y`, which cannot drift. The result is
  always editable; it is just more flexible where the page was already flexible.
- **Paint.** `background-color` becomes a solid fill; `linear-gradient()` and
  `radial-gradient()` become pen gradients with the direction converted (`180deg` CSS = pen 0);
  a colour under an image or gradient stays a separate lower fill. `border-radius` becomes
  `cornerRadius`, `box-shadow` becomes a shadow effect, and CSS borders become inner strokes.
- **Text.** Font family, size, weight, style, line height, letter spacing, alignment,
  decoration and link `href` carry over. Blocks that wrap use `textGrowth: "fixed-width"` at the
  measured width so line breaks stay put; single words use `auto`.
- **Images.** `<img>` and `background-image: url(...)` download into `<document>.assets/` named
  by content hash, and become image fills. `object-fit` maps to the pen image mode
  (`cover` → `fill`, `contain` → `fit`, `fill` → `stretch`).
- **SVG.** Inline `<svg>` is converted to real path layers (through the SVG importer), so marks
  and icons stay sharp and editable instead of becoming bitmaps.
- **Not imported:** `<canvas>`, `<video>` and cross-origin `<iframe>` content, pseudo-elements,
  CSS transforms and filters, and text split across inline styles beyond per-run layers. Those
  elements are reported in the response and left out rather than approximated.

Pen's `browser` imports into a browser node that stays live; study imports one snapshot. Take a
new import if the page changes — nothing keeps talking to the site.

Very large captures are clamped rather than split: screenshots at 4096 × 12000 CSS pixels, imports
at 4000 elements and depth 40 (the response says when it truncated).

## Examples

```js
// Read a page, then import the hero only.
browser({action: "load-page", url: "https://example.com"})
browser({action: "import-to-canvas", target: "query", querySelector: "header.hero"})
```

```js
// Close the loop on a locally served page: look at your own work.
browser({action: "load-page", url: "http://localhost:3000"})
browser({action: "return-screenshot"})
```

```js
// Read a component's styles without importing it.
browser({action: "return-element", target: "query", querySelector: ".pricing-card"})
```

## Environment

| Variable | Effect |
| --- | --- |
| `STUDY_BROWSER` | Path to the Chrome/Chromium binary, when it is not on `PATH`. |
| `STUDY_BROWSER_HEADED=1` | Show the browser window instead of running headless. |
| `STUDY_BROWSER_ARGS` | Extra command-line flags, space-separated. |
| `STUDY_NO_BROWSER=1` | Disable the tool entirely. |
| `STUDY_FETCH_HOSTS` | `load-page` may only navigate to these hosts; study's own image downloads obey it too (the page's scripts, CSS and images load normally in the browser). |
| `STUDY_ROOT` / `STUDY_WRITABLE` | `file://` pages, screenshots and downloaded assets must stay inside these. |

The browser keeps a profile in `~/.study/browser`, so sign-ins and cookies persist between
sessions. Its log is `~/.study/browser/browser.log` when it fails to start.
