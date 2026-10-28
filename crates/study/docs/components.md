# Components and instances

- Any node with `reusable: true` is a component. Keep components together (for example in a "Components"
  frame at the top of the document) and build screens from instances below them.
- An instance is a `ref` node: `{type: "ref", ref: componentId}`. It renders the component's whole subtree.
- Properties set on the ref override the component root (`width`, `fill`, `padding`, …).
- `descendants` customizes nodes inside the instance, keyed by the descendant's id:
  - `{ [labelId]: {content: "Save"} }` — property override
  - `{ [iconId]: {enabled: false} }` — hide a part (instances can't delete descendants)
  - `{ [slotId]: {children: [...]} }` — replace a descendant's children
  - `{ [slotId]: {type: "frame", ...} }` — replace the descendant with a new subtree
  - Inside nested instances, chain ids: `{ "nestedInstanceId/childId": {...} }`
- After creation, address instance parts with paths: `Update(instanceId + "/" + labelId, {content: "Save"})`,
  `Replace(instanceId + "/" + slotId, {...})`, `Get(instanceId + "/" + labelId)`.
- `Get(instanceId, {resolveInstances: true})` shows the expanded subtree with the exact paths to use.
- Editing the component updates every instance; overrides on an instance win over the component.
- `Copy(componentId, parent, {...})` creates an instance; `Copy(instanceId, ...)` duplicates the instance with its overrides.
- Frames marked `slot: [componentIds]` are intended to hold instances of those components.
