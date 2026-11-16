// Captures a page as plain JSON for the importer: geometry, a curated set of computed
// styles, text runs, images and inline SVG. Pure DOM reading — it changes nothing.
//
//   __studyCapture(target, maxNodes, maxDepth)
//
// `target` is the element to capture. The result is
//   { title, url, viewport, scroll, size, count, truncated, root }
// and every node is
//   { tag, name, rect: {x, y, w, h}, cs: {...}, text?, src?, svg?, kids?: [...] }
(() => {
  const target = window.__studyTarget || document.documentElement;
  const opts = window.__studyOpts || {};
  const maxNodes = opts.maxNodes || 4000;
  const maxDepth = opts.maxDepth || 40;
  const STYLE_KEYS = [
    "display", "position", "flexDirection", "flexWrap", "justifyContent", "alignItems",
    "gap", "rowGap", "columnGap", "paddingTop", "paddingRight", "paddingBottom", "paddingLeft",
    "backgroundColor", "backgroundImage", "backgroundSize", "color", "fontFamily", "fontSize",
    "fontWeight", "fontStyle", "lineHeight", "letterSpacing", "textAlign", "textTransform",
    "textDecorationLine", "whiteSpace", "overflow", "borderRadius", "borderTopWidth",
    "borderRightWidth", "borderBottomWidth", "borderLeftWidth", "borderTopColor", "boxShadow",
    "opacity", "visibility", "objectFit", "direction", "textOverflow", "textWrap",
  ];
  const SKIP = new Set(["HEAD", "SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE", "META", "LINK", "TITLE", "BASE", "BR", "WBR"]);

  const style = (el) => {
    const s = getComputedStyle(el);
    const o = {};
    for (const k of STYLE_KEYS) {
      const v = s[k];
      if (v !== undefined && v !== null && v !== "") o[k] = String(v);
    }
    return o;
  };

  const visible = (el, cs) => {
    if (SKIP.has(el.tagName)) return false;
    if (el.tagName === "INPUT" && el.type === "hidden") return false;
    if (cs.display === "none" || cs.visibility === "hidden" || cs.visibility === "collapse") return false;
    if (parseFloat(cs.opacity || "1") === 0) return false;
    return true;
  };

  const label = (el, cs, own) => {
    const aria = el.getAttribute("aria-label");
    if (aria) return aria.trim().slice(0, 80);
    const title = el.getAttribute("title");
    if (title) return title.trim().slice(0, 80);
    if (el.tagName === "IMG" && el.alt) return el.alt.trim().slice(0, 80);
    const text = (own || "").replace(/\s+/g, " ").trim();
    if (text) return text.slice(0, 60);
    const cls = typeof el.className === "string" ? el.className.split(/\s+/).filter(Boolean).slice(0, 2).join(".") : "";
    return (el.tagName.toLowerCase() + (el.id ? "#" + el.id : "") + (cls ? "." + cls : "")).slice(0, 80);
  };

  // Rects are page coordinates; the importer subtracts parents when it builds the tree.
  const box = (r) => ({
    x: Math.round((r.left + (window.scrollX || 0)) * 100) / 100,
    y: Math.round((r.top + (window.scrollY || 0)) * 100) / 100,
    w: Math.round(r.width * 100) / 100,
    h: Math.round(r.height * 100) / 100,
  });

  const ownText = (el) => {
    let s = "";
    for (const n of el.childNodes) if (n.nodeType === 3) s += n.nodeValue;
    return s.replace(/\s+/g, " ").trim();
  };

  const textRect = (node) => {
    const range = document.createRange();
    range.selectNodeContents(node);
    const r = range.getBoundingClientRect();
    if (r.width > 0 || r.height > 0) return r;
    const rects = range.getClientRects();
    return rects.length ? rects[0] : r;
  };

  const state = { count: 0, truncated: false, unprintable: 0 };

  const walk = (el, depth) => {
    if (state.count >= maxNodes || depth > maxDepth) {
      state.truncated = true;
      return null;
    }
    const cs = style(el);
    if (!visible(el, cs)) return null;
    const r = el.getBoundingClientRect();
    if (r.width < 0.5 && r.height < 0.5 && !el.textContent.trim()) return null;

    const own = ownText(el);
    const kidsEls = Array.from(el.children).filter((c) => !SKIP.has(c.tagName));
    const node = { tag: el.tagName.toLowerCase(), name: label(el, cs, own), rect: box(r), cs };
    state.count++;

    if (el.tagName === "IMG") {
      const src = el.currentSrc || el.src;
      if (src) node.src = src;
      node.name = label(el, cs, "") || "Image";
      return node;
    }
    if (el.tagName === "svg" || el.tagName === "SVG") {
      const markup = el.outerHTML;
      if (markup.length < 400_000) node.svg = markup;
      return node;
    }
    if (el.tagName === "CANVAS" || el.tagName === "VIDEO" || el.tagName === "IFRAME") {
      state.unprintable++;
      node.name = node.name + " (not imported)";
      return node;
    }

    // A block of text with no element children is a text node, not a frame.
    if (!kidsEls.length) {
      const text = (el.innerText || el.value || "").replace(/\s+/g, " ").trim();
      if (text) {
        node.text = text;
        // The ink box: a single-line label inside a centred or padded box starts here,
        // not at the element's edge.
        const range = document.createRange();
        range.selectNodeContents(el);
        const ink = range.getBoundingClientRect();
        if (ink.width > 0 || ink.height > 0) node.textRect = box(ink);
        // `text-wrap: balance` breaks lines at a width narrower than the box. The widest
        // rendered line is the width that reproduces those breaks.
        let widest = 0;
        for (const r of range.getClientRects()) widest = Math.max(widest, r.width);
        if (widest > 0) node.lineWidth = Math.round(widest * 100) / 100;
        if (el.tagName === "A" && el.getAttribute("href")) node.href = el.getAttribute("href");
        return node;
      }
      if (el.tagName === "INPUT" && el.value) {
        node.text = String(el.value);
        return node;
      }
      if (el.tagName === "HR") {
        node.tag = "hr";
        return node;
      }
    }

    const kids = [];
    for (const child of el.childNodes) {
      if (child.nodeType === 1) {
        const sub = walk(child, depth + 1);
        if (sub) kids.push(sub);
      } else if (child.nodeType === 3) {
        const text = (child.nodeValue || "").replace(/\s+/g, " ").trim();
        if (!text) continue;
        if (state.count >= maxNodes) {
          state.truncated = true;
          break;
        }
        const tr = textRect(child);
        const run = {
          tag: "#text",
          name: text.slice(0, 60),
          rect: box(tr),
          cs: { ...cs, display: "inline" },
          text,
        };
        if (el.tagName === "A" && el.getAttribute("href")) run.href = el.getAttribute("href");
        kids.push(run);
        state.count++;
      }
    }
    if (kids.length) node.kids = kids;
    return node;
  };

  const root = walk(target, 0);
  const de = document.documentElement;
  const scrollX = window.scrollX || 0;
  const scrollY = window.scrollY || 0;
  return {
    title: document.title || location.pathname || "Imported page",
    url: location.href,
    viewport: { w: window.innerWidth, h: window.innerHeight },
    scroll: { x: scrollX, y: scrollY },
    size: {
      w: Math.max(de.scrollWidth, window.innerWidth),
      h: Math.max(de.scrollHeight, window.innerHeight),
    },
    canvas: (() => {
      const b = getComputedStyle(document.body).backgroundColor;
      if (b && b !== "rgba(0, 0, 0, 0)" && b !== "transparent") return b;
      return getComputedStyle(document.documentElement).backgroundColor;
    })(),
    count: state.count,
    truncated: state.truncated,
    unprintable: state.unprintable,
    root,
  };
})();
