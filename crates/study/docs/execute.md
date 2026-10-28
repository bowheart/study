# The `execute` tool

`execute` runs a JavaScript snippet against the document in `filePath`.

- Each call runs in its own function scope: `const`/`let` are local. To keep a value for later calls,
  assign a plain global: `heroId = Insert(...)`.
- If the snippet throws, **every change from that call is reverted** and the globals it created are removed.
  The response carries an `editId`. Fix the snippet by calling `execute` again with `editId` and `edits`
  (`[{find, replace, all?}]`), not by resending it. If it fails again, keep patching under the same `editId`.
- Warnings (ignored properties, invisible text, …) are listed in the response. Fix them in the next call.
- The response ends with `name=id` lines for every node the call created, so you can reference them next time.
- Each successful call is one undo step for the user and is saved to the file immediately.
- Keep snippets focused (one section per call) and use loops, helpers and spreads instead of repetition.
- Never set `id` on new nodes; study generates ids.
- Set a `name` on every node you create.

## API

```ts
const document: string;   // the root; use as a parent for top-level nodes

// Changes
function Insert(parent: string, node: Node): string;                 // appends; returns the new id
function Copy(path: string, parent: string, overrides?: object): string;   // deep copy with new ids; returns its id
function Update(path: string, props: object): void;                  // merge properties (null removes one)
function Replace(path: string, node: Node): string;                  // swap a node for a new one; returns its id
function Move(path: string, parent?: string, index?: number): void;  // reparent and/or reorder
function Delete(path: string): void;
function SetVariables(vars: Record<string, VariableDef>, replace?: boolean): void;
function Generate(nodeId: string, type: "ai" | "svg" | "stock", prompt: string): void;   // study: only "svg", with SVG markup as the prompt

// Reading
function Get(path: string, options?: GetOptions): Node;                     // one node, children nested
function Get<T>(path: string, visit: Visit<T>, options?: GetOptions): T[];  // visit a subtree
function Get<T>(visit: Visit<T>, options?: GetOptions): T[];                // visit the whole document
function GetVariables(): { variables: Record<string, VariableDef>; themes: Record<string, string[]> };
function FindEmptySpace(input: { width: number; height: number; direction?: "top" | "right" | "bottom" | "left"; padding?: number; nodeId?: string }): { x: number; y: number; parentId?: string };

// Output
function Print(...values: unknown[]): void;          // one response line; strings raw, others as JSON
function TakeScreenshot(nodeIds: string[]): void;    // attaches PNGs of the nodes (as they look on the canvas)
function Export(nodeIds: string[], format: "png" | "jpeg" | "pdf" | "html-css" | "html-tailwind", outputPath: string, options?: ExportOptions): void;

type Visit<T> = (node: Node, ctx: Ctx) => T | undefined;   // returning undefined collects nothing

interface GetOptions {
  depth?: number;               // 0 = just the node; deeper children appear as "..."
  resolveVariables?: boolean;   // values instead of "$variable" references
  resolveInstances?: boolean;   // expand refs into their full subtree (ids become "instanceId/childId" paths)
  includePathGeometry?: boolean;   // path geometry is elided ("...") unless set
}

interface Ctx {
  node: Node;
  parentCtx?: Ctx;              // follow the chain for ancestors
  depth: number;
  index: number;                // position among siblings
  bounds: { x: number; y: number; width: number; height: number };   // laid-out box in the parent's coordinates
  problems?: "partially clipped" | "fully clipped";                  // sticks out of its parent
  skipChildren(): void;
}

interface ExportOptions {
  scale?: number;               // images, default 2
  quality?: number;             // jpeg 1-100
  includeHtmlScaffold?: boolean;   // html, default true
  includeLayerNames?: boolean;     // html data-name attributes, default true
  includeLayerIds?: boolean;       // html data-id attributes, default false
}
```

### Paths and targets

A `path` is a node id, or `instanceId/childId` to reach inside a component instance (deeper nesting
continues the chain: `instanceId/nestedInstanceId/childId`). Slashes never describe ordinary parent/child
structure. Targets are always strings — pass `node.id`, never the node object.

### Notes per function

- **Insert** adds one node (its `children`, if given, come along). For components, insert a `ref` and customize
  it with `descendants` (keys may be descendant ids or unique names) or later `Update("instance/child", …)`.
  To read the children of a node you just made: `Get(id, {depth: 1}).children`.
- **Copy** gives the copy and all its descendants new ids. Put property changes in `overrides`; change
  descendants with `overrides.descendants` (keys: original ids or unique names) — not with later Updates on the
  old ids. Copying a reusable node creates an instance (`ref`).
- **Update** can't change `id`, `type`, `ref` or `children`. On an instance path it records an override.
- **Replace** keeps the position in the parent. On an instance path it replaces that descendant inside the instance.
- **Delete** can't remove nodes inside instances; hide them with `Update(path, {enabled: false})`.
- **SetVariables** merges by default. Each definition is `{type, value}`; themed values are arrays of
  `{value, theme}` and new theme axes/values are registered automatically. Names never start with `$`;
  reference them as `"$name"`.
- **Get** without a visitor returns plain node data that can be passed back to Insert/Copy/Replace.
  With a visitor every node is visited top-down; it reflects changes made earlier in the same call.
  There is no visitor-less whole-document read.
- **FindEmptySpace** finds free space for top-level content (or next to `nodeId`, among its siblings).
  Insert at the returned `x`/`y` into `parentId` (or `document`).
- **TakeScreenshot** is rendered after the snippet finishes, so it shows this call's changes. Screenshot the
  smallest meaningful node; use `ctx.bounds` / `ctx.problems` for structural checks.
- **Export** writes files (paths relative to the .pen file). Images are `<nodeId>.<ext>` in the output
  directory; pdf is one `export.pdf` with a page per node; html formats write one file.

## Drawing SVG

`Generate(frameId, "svg", markup)` turns SVG markup into path nodes inside a frame. Study has no
generation service: you write the SVG. The user watches each shape being drawn in with a pen.

- Use it when the artwork itself is the point: a logo or mark, an illustration, a mascot, a custom
  diagram. Icons are `icon` nodes; badges, cards, blobs, glows and charts are frames and shapes.
- Insert a frame with a fixed `width` and `height` first. The drawing is scaled to fit it (centered,
  aspect kept), its shapes become `path` children in document order, and the frame's `layout` is set to `"none"`.
- Give the `<svg>` a `viewBox`. Supported: `path`, `rect`, `circle`, `ellipse`, `line`, `polyline`,
  `polygon`, groups and transforms, `use`, CSS styles, solid fills and strokes, linear and radial
  gradients, opacity, `fill-rule`. Not imported: text (convert it to paths), images, filters, masks,
  clip paths, patterns.
- Draw like an illustrator: a clear silhouette, a small deliberate palette, consistent stroke weights,
  shapes ordered back to front. Name shapes with `id` attributes; they become node names.
- Make reused artwork a component (`reusable: true` on the frame) rather than generating it again.

```js
const logo = Insert(document, {type: "frame", name: "Logo", width: 240, height: 240})
Generate(logo, "svg", `<svg viewBox="0 0 100 100" xmlns="http://www.w3.org/2000/svg">
  <circle id="Sun" cx="50" cy="42" r="22" fill="#F59E0B"/>
  <path id="Hills" d="M0 80 Q25 55 50 78 T100 72 V100 H0 Z" fill="#166534"/>
</svg>`)
```

## Examples

```js
Get(n => n.reusable && Print(n.id, n.name))                                   // list components
Get(screenId, (n, c) => Print(n.id, n.name, c.depth, c.bounds.width))          // compact outline
Get(screenId, (n, c) => c.problems && Print(n.name, "in", c.parentCtx?.node.name, c.problems))
Get(screenId, n => n.type === "text" && n.fontSize < 12 && Update(n.id, {fontSize: 12}), {resolveVariables: true})
Print(Get("Xk9f2", {depth: 2}))
```

```js
const pos = FindEmptySpace({width: 1440, height: 900, padding: 80})
pageId = Insert(document, {type: "frame", name: "Landing", x: pos.x, y: pos.y, width: 1440, layout: "vertical", padding: 48, gap: 32, fill: "$bg", clip: true, placeholder: true})
navId = Insert(pageId, {type: "frame", name: "Nav", width: "fill_container", gap: 24, alignItems: "center"})
const link = {type: "text", fontFamily: "Inter", fontSize: 14, fill: "$text-muted"}
for (const label of ["Product", "Pricing", "Docs"]) Insert(navId, {...link, name: label, content: label})
TakeScreenshot([pageId])
```

```js
cardId = Insert(document, {type: "frame", name: "Metric", reusable: true, layout: "vertical", gap: 4, padding: 16, x: 0, y: -400})
labelId = Insert(cardId, {type: "text", name: "Label", content: "Label", fontSize: 13, fill: "#6B7280"})
valueId = Insert(cardId, {type: "text", name: "Value", content: "0", fontSize: 28, fontWeight: "600", fill: "#111827"})
for (const [l, v] of [["Orders", "1,284"], ["Revenue", "$48.2K"]])
  Insert(rowId, {type: "ref", ref: cardId, name: l, width: "fill_container", descendants: {[labelId]: {content: l}, [valueId]: {content: v}}})
```
