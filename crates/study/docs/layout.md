# Layout

study's layout is a small flexbox: one axis, no wrapping, no margins, no percentages.

## Sizing

- Frames default to `layout: "horizontal"` and `fit_content` width and height.
- `fit_content` on a frame = the sum (main axis) or max (cross axis) of its children, plus padding and gaps.
- `fill_container` takes the parent's inner size on the cross axis, and splits the leftover main-axis space
  evenly between all `fill_container` siblings. It only works when the parent has `layout` horizontal/vertical.
- A `fill_container` child contributes nothing to a `fit_content` parent on that axis. If *every* child fills,
  the parent has nothing to size from and collapses to its fallback (`fit_content(N)`) or 0. Give the parent
  (or some child) a definite size on that axis.
- `x`/`y` are ignored for children of a flex parent unless they have `layoutPosition: "absolute"`.
  Children of `layout: "none"` frames are positioned by `x`/`y`.
- `justifyContent` distributes leftover main-axis space (it has no effect when a child fills).
  `alignItems` positions children on the cross axis (no stretch — use `fill_container` instead).
- Padding applies to all children. To offset a single child, wrap it in a frame with padding.
- Strokes never change layout.

## Text

- `textGrowth: "auto"` (default): a single line sized to the content. Never set width/height.
- `textGrowth: "fixed-width"` with `width: "fill_container"`: paragraphs and headings that wrap to the parent.
  The parent needs a definite width (fixed, or itself filling a definite parent).
- `textGrowth: "fixed-width-height"`: fixed box; use `textAlignVertical` to align inside it.
- `textAlign` only matters when the box is wider than the text (fixed widths).
- Never guess text sizes; let wrapping and layout do it.
- Text needs a `fill` to be visible.

## Common mistakes

```js
// Collapses to 0 wide: the parent fits its children, the only child fills the parent.
p = Insert(screen, {type: "frame", layout: "vertical"})
Insert(p, {type: "text", textGrowth: "fixed-width", width: "fill_container", content: "…"})
// Fix: give the parent a width (or fill_container inside a sized parent).

// Percentages and margins don't exist.
Insert(p, {type: "frame", width: "50%"})        // error
Insert(p, {type: "text", padding: 12})          // padding only applies to frames: wrap the text in a frame
```

## Grids and charts

- No wrapping: build grids from row frames (vertical frame of horizontal rows) with `fill_container` cells.
- Bar charts: a horizontal frame aligned to `end` with bars of computed heights; let layout place labels.
- Donuts: `ellipse` with `innerRadius`, `startAngle`, `sweepAngle`.
