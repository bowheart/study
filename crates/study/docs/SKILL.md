---
name: study
description: Create and edit designs (app screens, websites, dashboards, components) in .pen files on the study canvas. Use when the task involves reading or generating designs in a .pen file.
---

# study

study is a local infinite canvas for `.pen` design files: a tree of frames, shapes, text and
icons laid out with a simple flexbox model. You change the document by running small
JavaScript snippets with the `execute` tool; the user sees every change live and can undo
each `execute` call with Ctrl+Z.

## Reference

| Topic | When | File |
| --- | --- | --- |
| `.pen` schema | Always — the node types and properties you may use | [pen-schema.md](./pen-schema.md) |
| `execute` API | Always — functions available inside snippets | [execute.md](./execute.md) |
| Components | Reusable components, instances, overrides | [components.md](./components.md) |
| Layout | Sizing rules, text sizing, common layout mistakes | [layout.md](./layout.md) |
| Scripts, shaders, imports | Script nodes, shader fills (time, mouse, textures, SDF, backdrop), imported files | [scripts-and-shaders.md](./scripts-and-shaders.md) |

## Working rules

- Read before you write. Use `Get` with a visitor to find nodes, check bounds, and list components; don't guess ids.
- Only do what was asked. Moving, deleting, restyling or "cleaning up" things the user didn't mention is not welcome.
- The document may change between calls (the user can undo, or edit the file). If a node is missing or different from what you remember, re-read it instead of recreating it.
- Keep the document root tidy: only screens, component sheets and other major containers go directly in `document`. Each screen is a top-level frame (use `clip: true`).
- Place new top-level content with `FindEmptySpace` so it never overlaps existing frames.
- Give every node a human-readable `name`.
- Prefer flexbox layout (`layout: "horizontal" | "vertical"`) with `fit_content` / `fill_container` sizing over hardcoded sizes and absolute positions.
- When UI repeats, build a component (`reusable: true`) once and instance it with `ref` nodes.
- Prefer copying and adapting existing content over generating from scratch.
- Verify as you go: after finishing a section, check `ctx.problems` / `ctx.bounds` with a visitor, and use `TakeScreenshot` when visual fidelity matters. Fix issues by updating nodes in place — don't delete and rebuild.

## Style

- Avoid generic AI habits: a card around everything, gradients and shadows everywhere, identical repeated grids. Be intentional.
- Check text/background contrast.
- Use `icon` nodes for iconography (lucide by default) and frames/shapes/gradients for decoration.
- Any Google Font can be used by name in `fontFamily`.
- Vector artwork (logos, marks, illustrations, diagrams): `Generate(frameId, "svg", markup)`. Study has no generation service, so you are the illustrator: write the SVG yourself and pass the markup as the prompt. See "Drawing SVG" in execute.md.
- `Generate` with `"ai"` or `"stock"` (images) is not available in study. Use image files that exist on disk (image fills with a relative `url`), gradients, or shapes instead.
- `get_style` lists style references the user keeps in `~/.study/styles/`.
