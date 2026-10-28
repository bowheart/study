// The execute API, layered over a single host bridge: __host(name, jsonArgs) -> jsonResult.
var document = "document";
(function () {
  const host = globalThis.__host;
  const call = (name, args) => {
    const r = JSON.parse(host(name, JSON.stringify(args)));
    if (r.__error !== undefined) throw new Error(r.__error);
    return r.__value === null ? undefined : r.__value;
  };
  const def = (name, fn) =>
    Object.defineProperty(globalThis, name, { value: fn, writable: false, enumerable: false, configurable: false });

  def("Insert", (parent, nodeData) => call("Insert", [parent, nodeData]));
  def("Copy", (path, parent, copyNodeData) => call("Copy", [path, parent, copyNodeData === undefined ? null : copyNodeData]));
  def("Update", (path, updateData) => call("Update", [path, updateData]));
  def("Replace", (path, nodeData) => call("Replace", [path, nodeData]));
  def("Move", (path, parent, index) => call("Move", [path, parent === undefined ? null : parent, index === undefined ? null : index]));
  def("Delete", (path) => call("Delete", [path]));
  def("Generate", (nodeId, type, prompt) => call("Generate", [nodeId, type, prompt]));
  def("SetVariables", (variables, replace) => call("SetVariables", [variables, !!replace]));
  def("GetVariables", () => call("GetVariables", []));
  def("FindEmptySpace", (input) => call("FindEmptySpace", [input || {}]));
  def("TakeScreenshot", (nodeIds) => call("TakeScreenshot", [nodeIds]));
  def("Export", (nodeIds, format, outputPath, options) => call("Export", [nodeIds, format, outputPath, options || {}]));
  def("Print", (...values) =>
    call("Print", [
      values
        .map((v) => {
          if (typeof v === "string") return v;
          if (v === undefined) return "undefined";
          try {
            return JSON.stringify(v);
          } catch (e) {
            return String(v);
          }
        })
        .join(" "),
    ])
  );

  def("Get", (a, b, c) => {
    let path, visit, opts;
    if (typeof a === "function") {
      visit = a;
      opts = b;
    } else {
      path = a;
      if (typeof b === "function") {
        visit = b;
        opts = c;
      } else {
        opts = b;
      }
    }
    opts = opts || {};
    if (path !== undefined && typeof path !== "string")
      throw new Error("Get: path must be an id string; when holding a node from Get, pass its .id");
    if (!visit) {
      if (path === undefined || path === "document")
        throw new Error("Get needs a node id, or a visitor function to walk the document (there is no whole-document read)");
      return call("GetNode", [path, opts]);
    }
    const roots = call("GetTree", [path === undefined ? "document" : path, opts]);
    const build = (t) => {
      const n = t.d;
      if (t.c) n.children = t.c.map(build);
      else if (t.e) n.children = "...";
      t.n = n;
      return n;
    };
    roots.forEach(build);
    const out = [];
    const walk = (t, parentCtx, depth, index) => {
      let skip = false;
      const ctx = {
        node: t.n,
        parentCtx,
        depth,
        index,
        bounds: t.b ? { x: t.b[0], y: t.b[1], width: t.b[2], height: t.b[3] } : undefined,
        skipChildren() {
          skip = true;
        },
      };
      if (t.p) ctx.problems = t.p;
      const r = visit(t.n, ctx);
      if (r !== undefined) out.push(r);
      if (!skip && t.c) t.c.forEach((child, i) => walk(child, ctx, depth + 1, i));
    };
    roots.forEach((t, i) => walk(t, undefined, 0, i));
    return out;
  });
})();
