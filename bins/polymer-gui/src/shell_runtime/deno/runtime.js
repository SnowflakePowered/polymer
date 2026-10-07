(() => {
  "use strict";

  const { ops, runMicrotasks, webEvents } = globalThis.__polymerRuntime;
  const registerPolymerSurface = globalThis.__polymerApi.registerSurface;
  const { defineEventHandler } = webEvents;
  const eventDispatch = globalThis.__polymerCreateEventDispatch(webEvents, ops);
  delete globalThis.__polymerCreateEventDispatch;
  const { MouseEvent, PointerEvent, WheelEvent, KeyboardEvent, FocusEvent, InputEvent, CompositionEvent, dispatch, dispatchNativeEvent } = eventDispatch;
  delete globalThis.__polymerRuntime;
  const wrappers = new Map();
  const pointerCaptures = new Map();
  let documentNode = null;
  function sendShellMessage(message) {
    ops.op_post_message(message);
  }

  function dashToCamel(value) {
    return value.replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
  }

  function camelToDash(value) {
    return value.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`);
  }

  function parseDeclarations(value) {
    const declarations = new Map();
    for (const part of String(value || "").split(";")) {
      const split = part.indexOf(":");
      if (split < 0) continue;
      const name = part.slice(0, split).trim();
      if (name) declarations.set(name, part.slice(split + 1).trim());
    }
    return declarations;
  }

  function serializeDeclarations(declarations) {
    return [...declarations].map(([name, value]) => `${name}: ${value}`).join("; ");
  }

  class DOMTokenList {
    constructor(element, attribute) {
      this.element = element;
      this.attribute = attribute;
    }

    #tokens() {
      return (this.element.getAttribute(this.attribute) || "").split(/\s+/).filter(Boolean);
    }

    #write(tokens) {
      this.element.setAttribute(this.attribute, [...new Set(tokens)].join(" "));
    }

    contains(token) {
      return this.#tokens().includes(String(token));
    }

    add(...tokens) {
      this.#write([...this.#tokens(), ...tokens.map(String)]);
    }

    remove(...tokens) {
      const removed = new Set(tokens.map(String));
      this.#write(this.#tokens().filter((token) => !removed.has(token)));
    }

    toggle(token, force) {
      token = String(token);
      const present = this.contains(token);
      const next = force === undefined ? !present : Boolean(force);
      if (next && !present) this.add(token);
      if (!next && present) this.remove(token);
      return next;
    }

    replace(oldToken, newToken) {
      const tokens = this.#tokens();
      const index = tokens.indexOf(String(oldToken));
      if (index < 0) return false;
      tokens[index] = String(newToken);
      this.#write(tokens);
      return true;
    }

    get value() {
      return this.element.getAttribute(this.attribute) || "";
    }

    toString() {
      return this.value;
    }

    [Symbol.iterator]() {
      return this.#tokens()[Symbol.iterator]();
    }
  }

  class CSSStyleDeclaration {
    constructor(element) {
      this.element = element;
      return new Proxy(this, {
        get(target, property, receiver) {
          if (typeof property !== "string" || property in target) {
            return Reflect.get(target, property, receiver);
          }
          return target.getPropertyValue(camelToDash(property));
        },
        set(target, property, value, receiver) {
          if (typeof property !== "string" || property in target) {
            return Reflect.set(target, property, value, receiver);
          }
          target.setProperty(camelToDash(property), value);
          return true;
        },
        has(target, property) {
          return Reflect.has(target, property) ||
            (typeof property === "string" &&
              target.getPropertyValue(camelToDash(property)) !== "");
        },
      });
    }

    getPropertyValue(name) {
      return parseDeclarations(this.element.getAttribute("style")).get(String(name)) || "";
    }

    setProperty(name, value, priority = "") {
      if (value === null || String(value) === "") {
        this.removeProperty(name);
        return;
      }
      const declarations = parseDeclarations(this.element.getAttribute("style"));
      const suffix = String(priority).toLowerCase() === "important" ? " !important" : "";
      declarations.set(String(name), `${String(value)}${suffix}`);
      this.element.setAttribute("style", serializeDeclarations(declarations));
    }

    removeProperty(name) {
      const declarations = parseDeclarations(this.element.getAttribute("style"));
      const previous = declarations.get(String(name)) || "";
      declarations.delete(String(name));
      this.element.setAttribute("style", serializeDeclarations(declarations));
      return previous;
    }

    get cssText() {
      return this.element.getAttribute("style") || "";
    }

    set cssText(value) {
      this.element.setAttribute("style", String(value));
    }

    get length() {
      return parseDeclarations(this.element.getAttribute("style")).size;
    }

    item(index) {
      return [...parseDeclarations(this.element.getAttribute("style")).keys()][index] || "";
    }
  }

  const mutationObservers = new Set();
  function notifyMutation(target, record) {
    for (const observer of mutationObservers) observer.__enqueue({ target, ...record });
  }
  class MutationObserver {
    constructor(callback) { this.callback = callback; this.records = []; this.targets = new Map(); this.queued = false; }
    observe(target, options = {}) { this.targets.set(target, options); mutationObservers.add(this); }
    disconnect() { this.targets.clear(); this.records.length = 0; mutationObservers.delete(this); }
    takeRecords() { return this.records.splice(0); }
    __enqueue(record) {
      const interested = [...this.targets].some(([target, options]) => record.target === target || (options.subtree && target.contains(record.target)));
      if (!interested) return;
      this.records.push(record);
      if (this.queued) return;
      this.queued = true;
      queueMicrotask(() => { this.queued = false; const records = this.takeRecords(); if (records.length) this.callback(records, this); });
    }
  }
  const resizeObservers = new Set();
  class ResizeObserver {
    constructor(callback) { this.callback = callback; this.targets = new Set(); }
    observe(target) { this.targets.add(target); resizeObservers.add(this); }
    unobserve(target) { this.targets.delete(target); if (!this.targets.size) resizeObservers.delete(this); }
    disconnect() { this.targets.clear(); resizeObservers.delete(this); }
  }
  class IntersectionObserver {
    constructor(callback, options = {}) {
      this.callback = callback;
      this.root = options.root || null;
      this.rootMargin = options.rootMargin || "0px";
      this.thresholds = Array.isArray(options.threshold)
        ? options.threshold
        : [options.threshold ?? 0];
      this.targets = new Set();
    }
    observe(target) {
      this.targets.add(target);
      queueMicrotask(() => {
        if (!this.targets.has(target)) return;
        const rect = target.getBoundingClientRect();
        const visible = target.isConnected && rect.width > 0 && rect.height > 0;
        this.callback([{
          time: performance.now(),
          target,
          rootBounds: null,
          boundingClientRect: rect,
          intersectionRect: visible ? rect : Object.freeze({
            x: 0, y: 0, width: 0, height: 0,
            left: 0, top: 0, right: 0, bottom: 0,
          }),
          isIntersecting: visible,
          intersectionRatio: visible ? 1 : 0,
        }], this);
      });
    }
    unobserve(target) { this.targets.delete(target); }
    disconnect() { this.targets.clear(); }
    takeRecords() { return []; }
  }

  const HTML_NAMESPACE = "http://www.w3.org/1999/xhtml";
  const PARENT = 0;
  const FIRST_CHILD = 1;
  const LAST_CHILD = 2;
  const NEXT_SIBLING = 3;
  const PREVIOUS_SIBLING = 4;

  function nodeFor(id) {
    if (!id) return null;
    const existing = wrappers.get(id);
    if (existing) return existing;
    const info = ops.op_dom_node_info(id);
    if (!info) return null;
    const node = makeNode(id, info);
    wrappers.set(id, node);
    if (node instanceof HTMLTrackedSurfaceElement) registerPolymerSurface(node, id);
    return node;
  }

  function selectorError(error) {
    return new DOMException(error?.message || String(error), "SyntaxError");
  }

  function query(scope, selector, all) {
    try {
      return ops.op_dom_query(scope.__id, String(selector), all).map(nodeFor).filter(Boolean);
    } catch (error) {
      throw selectorError(error);
    }
  }

  function asNode(item) {
    return item instanceof Node ? item : documentNode.createTextNode(String(item));
  }

  class Node extends EventTarget {
    constructor(id) {
      super();
      this.__id = id;
      this.__notifiedScroll = [0, 0];
    }

    __relative(relation) {
      return nodeFor(ops.op_dom_relative(this.__id, relation));
    }

    get parentNode() {
      return this.__relative(PARENT);
    }

    get nodeType() {
      return 0;
    }

    dispatchEvent(event) {
      // deno_web's generic EventTarget fast path returns immediately when the
      // target itself has no listener. DOM Nodes are different: an event can
      // still be observed by delegated listeners on any ancestor, Document,
      // or Window. Enter the full DOM dispatcher unconditionally for Nodes.
      return dispatchNativeEvent(this, event);
    }

    get parentElement() {
      const parent = this.parentNode;
      return parent instanceof Element ? parent : null;
    }

    get ownerDocument() {
      return this === documentNode ? null : documentNode;
    }

    get childNodes() {
      return ops.op_dom_children(this.__id).map(nodeFor).filter(Boolean);
    }

    get firstChild() {
      return this.__relative(FIRST_CHILD);
    }

    get lastChild() {
      return this.__relative(LAST_CHILD);
    }

    get nextSibling() {
      return this.__relative(NEXT_SIBLING);
    }

    get previousSibling() {
      return this.__relative(PREVIOUS_SIBLING);
    }

    __checkInsertion(child) {
      if (!(child instanceof Node)) throw new TypeError("child must be a Node");
      if (child.contains(this)) {
        throw new DOMException("The new child is an ancestor of the parent", "HierarchyRequestError");
      }
    }

    appendChild(child) {
      this.__checkInsertion(child);
      if (child instanceof DocumentFragment) {
        for (const item of child.childNodes) this.appendChild(item);
        return child;
      }
      ops.op_dom_append_child(this.__id, child.__id);
      notifyMutation(this, { type: "childList", addedNodes: [child], removedNodes: [] });
      return child;
    }

    insertBefore(child, before) {
      if (before === null || before === undefined) return this.appendChild(child);
      this.__checkInsertion(child);
      if (!(before instanceof Node)) throw new TypeError("insertBefore expects Nodes");
      if (before.parentNode !== this) {
        throw new DOMException("The reference node is not a child", "NotFoundError");
      }
      if (child instanceof DocumentFragment) {
        for (const item of child.childNodes) this.insertBefore(item, before);
        return child;
      }
      ops.op_dom_insert_before(this.__id, child.__id, before.__id);
      notifyMutation(this, { type: "childList", addedNodes: [child], removedNodes: [] });
      return child;
    }

    removeChild(child) {
      if (!(child instanceof Node) || child.parentNode !== this) {
        throw new DOMException("The node is not a child", "NotFoundError");
      }
      ops.op_dom_remove_child(this.__id, child.__id);
      notifyMutation(this, { type: "childList", addedNodes: [], removedNodes: [child] });
      return child;
    }

    replaceChild(child, replaced) {
      this.insertBefore(child, replaced);
      this.removeChild(replaced);
      return replaced;
    }

    remove() {
      this.parentNode?.removeChild(this);
    }

    append(...items) {
      for (const item of items) this.appendChild(asNode(item));
    }

    prepend(...items) {
      const first = this.firstChild;
      for (const item of items) this.insertBefore(asNode(item), first);
    }

    before(...items) {
      const parent = this.parentNode;
      if (!parent) return;
      for (const item of items) parent.insertBefore(asNode(item), this);
    }

    after(...items) {
      const parent = this.parentNode;
      if (!parent) return;
      const reference = this.nextSibling;
      for (const item of items) parent.insertBefore(asNode(item), reference);
    }

    replaceWith(...items) {
      this.before(...items);
      this.remove();
    }

    cloneNode(deep = false) {
      return nodeFor(ops.op_dom_clone(this.__id, Boolean(deep)));
    }

    contains(other) {
      for (let current = other; current instanceof Node; current = current.parentNode) {
        if (current === this) return true;
      }
      return false;
    }

    get isConnected() {
      return this === documentNode || ops.op_dom_is_connected(this.__id);
    }

    getRootNode() {
      let root = this;
      // Window participates above Document in the event path, but is not a
      // DOM Node and therefore is not the tree root used by the native
      // deno_web dispatch algorithm.
      while (root.parentNode instanceof Node) root = root.parentNode;
      return root;
    }

    get textContent() {
      return ops.op_dom_text_content(this.__id);
    }

    set textContent(value) {
      for (const child of this.childNodes) this.removeChild(child);
      const text = String(value ?? "");
      if (text) this.appendChild(documentNode.createTextNode(text));
    }
  }

  class DocumentFragment extends Node {
    constructor(id = ops.op_dom_create_fragment()) {
      super(id);
      if (!wrappers.has(id)) wrappers.set(id, this);
    }
    get nodeType() { return 11; }
    get nodeName() { return "#document-fragment"; }
    querySelectorAll(selector) { return query(this, selector, true); }
    querySelector(selector) { return query(this, selector, false)[0] || null; }
  }

  function cssPixel(value) {
    const match = /^\s*(-?(?:\d+\.?\d*|\.\d+))px\s*$/.exec(String(value || ""));
    return match ? Number(match[1]) : null;
  }

  function inlineTranslation(element) {
    const transform = element.style.getPropertyValue("transform");
    const match = /^\s*translate(?:3d)?\(\s*([^,]+),\s*([^,)]+)/.exec(transform);
    return match ? { x: cssPixel(match[1]) || 0, y: cssPixel(match[2]) || 0 } : { x: 0, y: 0 };
  }

  function translatedClientRect(element) {
    let [x, y, width, height] = ops.op_dom_rect(element.__id);
    // Blitz geometry contains layout coordinates. CSS transforms do not alter
    // layout, but browsers include the element's and its ancestors' transforms
    // in getBoundingClientRect(). Keep this calculation synchronous so a drag
    // state machine can write its transform and read the new box in one event.
    for (let current = element; current instanceof Element; current = current.parentElement) {
      const translation = inlineTranslation(current);
      x += translation.x;
      y += translation.y;
    }
    return { x, y, width, height };
  }

  function pointerCaptureTarget(pointerId) {
    const id = Number(pointerId) || 1;
    return pointerCaptures.get(id) || null;
  }

  class Element extends Node {
    constructor(id, info) {
      super(id);
      this.namespaceURI = info.namespace || HTML_NAMESPACE;
      this.localName = String(info.tag || "");
      this.tagName = this.namespaceURI === HTML_NAMESPACE ? this.localName.toUpperCase() : this.localName;
      this.nodeName = this.tagName;
      this.style = new CSSStyleDeclaration(this);
      this.classList = new DOMTokenList(this, "class");
      this.__capturedPointers = new Set();
    }

    __attributeName(name) {
      name = String(name);
      return this.namespaceURI === HTML_NAMESPACE ? name.toLowerCase() : name;
    }

    get __scroll() {
      return ops.op_dom_scroll_metrics(this.__id);
    }

    get id() {
      return this.getAttribute("id") || "";
    }

    get nodeType() {
      return 1;
    }

    set id(value) {
      this.setAttribute("id", value);
    }

    get className() {
      return this.getAttribute("class") || "";
    }

    set className(value) {
      this.setAttribute("class", value);
    }

    get attributes() {
      return ops.op_dom_attributes(this.__id).map(([name, value]) => ({ name, value }));
    }

    get nextElementSibling() {
      for (let sibling = this.nextSibling; sibling; sibling = sibling.nextSibling) {
        if (sibling instanceof Element) return sibling;
      }
      return null;
    }

    get previousElementSibling() {
      for (let sibling = this.previousSibling; sibling; sibling = sibling.previousSibling) {
        if (sibling instanceof Element) return sibling;
      }
      return null;
    }

    get children() {
      return this.childNodes.filter((node) => node instanceof Element);
    }

    get firstElementChild() {
      return this.children[0] || null;
    }

    get lastElementChild() {
      return this.children.at(-1) || null;
    }

    insertAdjacentElement(position, element) {
      if (!(element instanceof Element)) {
        throw new TypeError("insertAdjacentElement expects an Element");
      }
      switch (String(position).toLowerCase()) {
        case "beforebegin":
          if (!this.parentNode) return null;
          this.parentNode.insertBefore(element, this);
          break;
        case "afterbegin":
          this.insertBefore(element, this.firstChild);
          break;
        case "beforeend":
          this.appendChild(element);
          break;
        case "afterend":
          if (!this.parentNode) return null;
          this.parentNode.insertBefore(element, this.nextSibling);
          break;
        default:
          throw new DOMException(
            `Invalid position '${position}'`,
            "SyntaxError",
          );
      }
      return element;
    }

    get dataset() {
      const element = this;
      return new Proxy({}, {
        get(_, key) {
          if (typeof key !== "string") return undefined;
          return element.getAttribute(`data-${camelToDash(key)}`) ?? undefined;
        },
        set(_, key, value) {
          element.setAttribute(`data-${camelToDash(String(key))}`, String(value));
          return true;
        },
        deleteProperty(_, key) {
          element.removeAttribute(`data-${camelToDash(String(key))}`);
          return true;
        },
        has(_, key) {
          return typeof key === "string" && element.hasAttribute(`data-${camelToDash(key)}`);
        },
        ownKeys() {
          return element.attributes
            .map(({ name }) => name)
            .filter((name) => name.startsWith("data-"))
            .map((name) => dashToCamel(name.slice(5)));
        },
        getOwnPropertyDescriptor(_, key) {
          if (typeof key !== "string") return undefined;
          const value = element.getAttribute(`data-${camelToDash(key)}`);
          if (value === null) return undefined;
          return { value, writable: true, enumerable: true, configurable: true };
        },
      });
    }

    hasAttribute(name) {
      return this.getAttribute(name) !== null;
    }

    getAttribute(name) {
      return ops.op_dom_get_attribute(this.__id, this.__attributeName(name)) ?? null;
    }

    setAttribute(name, value) {
      name = this.__attributeName(name);
      ops.op_dom_set_attribute(this.__id, name, String(value));
      notifyMutation(this, { type: "attributes", attributeName: name });
    }

    getAttributeNS(_namespace, name) {
      return this.getAttribute(name);
    }

    setAttributeNS(_namespace, name, value) {
      this.setAttribute(name, value);
    }

    removeAttribute(name) {
      name = this.__attributeName(name);
      if (!this.hasAttribute(name)) return;
      ops.op_dom_remove_attribute(this.__id, name);
      notifyMutation(this, { type: "attributes", attributeName: name });
    }

    removeAttributeNS(_namespace, name) {
      this.removeAttribute(name);
    }

    toggleAttribute(name, force) {
      const present = this.hasAttribute(name);
      const next = force === undefined ? !present : Boolean(force);
      if (next && !present) this.setAttribute(name, "");
      if (!next && present) this.removeAttribute(name);
      return next;
    }

    matches(selector) {
      try {
        return ops.op_dom_matches(this.__id, String(selector));
      } catch (error) {
        throw selectorError(error);
      }
    }

    closest(selector) {
      try {
        return nodeFor(ops.op_dom_closest(this.__id, String(selector)));
      } catch (error) {
        throw selectorError(error);
      }
    }

    querySelectorAll(selector) {
      return query(this, selector, true);
    }

    querySelector(selector) {
      return query(this, selector, false)[0] || null;
    }

    getBoundingClientRect() {
      const { x, y, width, height } = translatedClientRect(this);
      return Object.freeze({
        x, y, width, height,
        left: x, top: y, right: x + width, bottom: y + height,
        toJSON() { return { x, y, width, height, left: x, top: y, right: x + width, bottom: y + height }; },
      });
    }

    getClientRects() {
      return [this.getBoundingClientRect()];
    }

    get offsetParent() {
      return this.parentElement || documentNode?.body || null;
    }

    get offsetLeft() {
      return ops.op_dom_rect(this.__id)[0];
    }

    get offsetTop() {
      return ops.op_dom_rect(this.__id)[1];
    }

    get offsetWidth() {
      return ops.op_dom_rect(this.__id)[2];
    }

    get offsetHeight() {
      return ops.op_dom_rect(this.__id)[3];
    }

    get clientWidth() {
      return this.__scroll.clientWidth;
    }

    get clientHeight() {
      return this.__scroll.clientHeight;
    }

    get clientLeft() { return this.__scroll.clientLeft; }
    get clientTop() { return this.__scroll.clientTop; }

    get scrollLeft() {
      return this.__scroll.left;
    }
    set scrollLeft(value) { this.scrollTo({ left: value, behavior: "instant" }); }

    get scrollTop() {
      return this.__scroll.top;
    }
    set scrollTop(value) { this.scrollTo({ top: value, behavior: "instant" }); }

    get scrollWidth() { return this.__scroll.width; }
    get scrollHeight() { return this.__scroll.height; }
    scroll(...args) { this.scrollTo(...args); }
    scrollTo(left, top) { this.__requestScroll("to", left, top); }
    scrollBy(left, top) { this.__requestScroll("by", left, top); }
    __requestScroll(kind, left, top) {
      const options = typeof left === "object" ? (left ?? {}) : { left, top };
      const behavior = options.behavior ?? "auto";
      if (!["auto", "instant", "smooth"].includes(behavior)) throw new TypeError("Invalid scroll behavior");
      const number = value => Number.isFinite(Number(value)) ? Number(value) : 0;
      const x = options.left === undefined ? (kind === "by" ? 0 : null) : number(options.left);
      const y = options.top === undefined ? (kind === "by" ? 0 : null) : number(options.top);
      ops.op_dom_scroll(this.__id, { kind, left: x, top: y, behavior });
    }
    scrollIntoView(options = {}) {
      if (typeof options === "boolean") options = { block: options ? "start" : "end" };
      options ??= {};
      const block = options.block ?? "start", inline = options.inline ?? "nearest";
      const alignments = ["start", "center", "end", "nearest"];
      if (!alignments.includes(block) || !alignments.includes(inline)) throw new TypeError("Invalid scroll alignment");
      ops.op_dom_scroll(this.__id, { kind: "intoView", block, inline, nearest: options.container === "nearest" });
    }

    setPointerCapture(pointerId) {
      const id = Number(pointerId);
      this.__capturedPointers.add(id);
      pointerCaptures.set(id, this);
    }
    releasePointerCapture(pointerId) {
      const id = Number(pointerId);
      this.__capturedPointers.delete(id);
      if (pointerCaptures.get(id) === this) pointerCaptures.delete(id);
    }
    hasPointerCapture(pointerId) { return this.__capturedPointers.has(Number(pointerId)); }

    get title() { return this.getAttribute("title") || ""; }
    set title(value) { this.setAttribute("title", value); }
    get role() { return this.getAttribute("role") || ""; }
    set role(value) { this.setAttribute("role", value); }
    get tabIndex() { return Number(this.getAttribute("tabindex") ?? -1); }
    set tabIndex(value) { this.setAttribute("tabindex", value); }
    get value() { return this.__value ?? (this.getAttribute("value") || ""); }
    set value(value) {
      value = String(value ?? "");
      if (value === this.value) return;
      this.__value = value;
      this.setAttribute("value", value);
    }
    get checked() { return this.hasAttribute("checked"); }
    set checked(value) { this.toggleAttribute("checked", Boolean(value)); }
    get hidden() { return this.hasAttribute("hidden"); }
    set hidden(value) { this.toggleAttribute("hidden", Boolean(value)); }
    get disabled() { return this.hasAttribute("disabled"); }
    set disabled(value) { this.toggleAttribute("disabled", Boolean(value)); }
    get readOnly() { return this.hasAttribute("readonly"); }
    set readOnly(value) { this.toggleAttribute("readonly", Boolean(value)); }
    get required() { return this.hasAttribute("required"); }
    set required(value) { this.toggleAttribute("required", Boolean(value)); }

    focus(options = {}) {
      const previous = nodeFor(ops.op_dom_focused());
      if (previous === this || !ops.op_dom_focus(this.__id)) return;
      if (previous) { dispatch(previous, new Event("blur")); dispatch(previous, new Event("focusout", { bubbles: true })); }
      dispatch(this, new Event("focus")); dispatch(this, new Event("focusin", { bubbles: true }));
      if (!options.preventScroll) this.scrollIntoView({ block: "nearest", inline: "nearest" });
    }

    blur() { if (nodeFor(ops.op_dom_focused()) === this) documentNode?.body?.focus(); }

    click() {
      if (this.disabled) return;
      this.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, composed: true }));
    }

    get innerHTML() {
      return this.textContent;
    }

    set innerHTML(value) {
      ops.op_dom_set_inner_html(this.__id, String(value ?? ""));
      notifyMutation(this, { type: "childList", addedNodes: this.childNodes, removedNodes: [] });
    }
  }

  class HTMLElement extends Element {}
  class HTMLImageElement extends HTMLElement {
    get naturalWidth() { return ops.op_dom_image_size(this.__id)[0]; }
    get naturalHeight() { return ops.op_dom_image_size(this.__id)[1]; }
    get complete() { return this.naturalWidth > 0 || !this.getAttribute("src"); }
    __syncImage() {
      const size = ops.op_dom_image_size(this.__id);
      const key = `${this.getAttribute("src")}:${size.join(":")}`;
      if (key === this.__loadedImage) return;
      this.__loadedImage = key;
      if (size[0] > 0 && size[1] > 0) queueMicrotask(() => {
        if (this.__loadedImage === key && this.isConnected) this.dispatchEvent(new Event("load"));
      });
    }
  }
  class HTMLHeadElement extends HTMLElement {}
  class HTMLBodyElement extends HTMLElement {}
  class HTMLTemplateElement extends HTMLElement {
    get content() { return nodeFor(ops.op_dom_template_content(this.__id)); }
    get innerHTML() { return this.content.textContent; }
    set innerHTML(value) { ops.op_dom_set_inner_html(this.content.__id, String(value ?? "")); }
  }
  class CharacterData extends Node {
    get data() {
      return ops.op_dom_text(this.__id);
    }

    get nodeValue() {
      return this.data;
    }

    set nodeValue(value) {
      this.data = value ?? "";
    }

    get textContent() {
      return this.data;
    }

    set textContent(value) {
      this.data = String(value ?? "");
    }

    get length() {
      return this.data.length;
    }
  }

  class Text extends CharacterData {
    get nodeType() {
      return 3;
    }

    get nodeName() {
      return "#text";
    }

    get data() {
      return super.data;
    }

    set data(value) {
      ops.op_dom_set_text(this.__id, String(value));
      notifyMutation(this, { type: "characterData" });
    }
  }

  class Comment extends CharacterData {
    get nodeType() {
      return 8;
    }

    get nodeName() {
      return "#comment";
    }
  }

  class HTMLTrackedSurfaceElement extends HTMLElement {
    get preferredSize() {
      return Object.freeze({
        width: Number(this.dataset.preferredWidth) || 0,
        height: Number(this.dataset.preferredHeight) || 0,
      });
    }
  }

  class Document extends Node {
    get nodeType() {
      return 9;
    }

    get parentNode() {
      return globalThis;
    }

    get nodeName() {
      return "#document";
    }

    get defaultView() {
      return globalThis;
    }

    get textContent() {
      return null;
    }

    get scrollingElement() { return this.documentElement; }

    get documentElement() {
      return this.childNodes.find((node) => node instanceof Element && node.localName === "html") || null;
    }

    get head() {
      return this.documentElement?.querySelector("head") || null;
    }

    get body() {
      return this.documentElement?.querySelector("body") || null;
    }

    get readyState() {
      return "complete";
    }

    get activeElement() {
      const focused = nodeFor(ops.op_dom_focused());
      return focused && focused !== this.documentElement ? focused : this.body;
    }

    get visibilityState() {
      return "visible";
    }

    get hidden() {
      return false;
    }

    createElement(tag) {
      return this.createElementNS(HTML_NAMESPACE, tag);
    }

    createElementNS(namespace, tag) {
      namespace = namespace || HTML_NAMESPACE;
      tag = namespace === HTML_NAMESPACE ? String(tag).toLowerCase() : String(tag);
      return nodeFor(ops.op_dom_create_element(tag, namespace));
    }

    createDocumentFragment() { return nodeFor(ops.op_dom_create_fragment()); }

    importNode(node, deep = false) { return node.cloneNode(deep); }

    createTextNode(text) {
      return nodeFor(ops.op_dom_create_text(String(text)));
    }

    createComment(data = "") {
      return nodeFor(ops.op_dom_create_comment(String(data)));
    }

    getElementById(id) {
      return nodeFor(ops.op_dom_element_by_id(String(id)));
    }

    querySelectorAll(selector) {
      return query(this, selector, true);
    }

    querySelector(selector) {
      return query(this, selector, false)[0] || null;
    }

    getElementsByTagName(tag) {
      return this.querySelectorAll(String(tag));
    }
  }

  function makeNode(id, info) {
    switch (info.kind) {
      case "document": return new Document(id);
      case "text": return new Text(id);
      case "comment": return new Comment(id);
      case "fragment": return new DocumentFragment(id);
    }
    switch (info.tag) {
      case "tracked-surface": return new HTMLTrackedSurfaceElement(id, info);
      case "template": return new HTMLTemplateElement(id, info);
      case "img": return new HTMLImageElement(id, info);
      case "head": return new HTMLHeadElement(id, info);
      case "body": return new HTMLBodyElement(id, info);
      default: return new HTMLElement(id, info);
    }
  }

  for (const type of ["mousedown", "mouseup", "mousemove", "click", "keydown", "keyup", "scroll"]) {
    defineEventHandler(Element.prototype, type);
    defineEventHandler(Document.prototype, type);
    defineEventHandler(globalThis, type);
  }
  defineEventHandler(globalThis, "message");

  let dimensions = { width: 0, height: 0 };
  let deviceScaleFactor = 1;
  const windowObject = globalThis;

  function setDeviceScaleFactor(value) {
    deviceScaleFactor = Number.isFinite(value) && value > 0 ? value : 1;
  }

  function syncLayout(snapshot) {
    dimensions = { width: snapshot.width, height: snapshot.height };
    setDeviceScaleFactor(snapshot.devicePixelRatio);
    const scrolled = [];
    for (const id of snapshot.scrolled) {
      const node = wrappers.get(id);
      if (!(node instanceof Element)) continue;
      const { left, top } = node.__scroll;
      if (node.__notifiedScroll[0] === left && node.__notifiedScroll[1] === top) continue;
      node.__notifiedScroll = [left, top];
      scrolled.push(node);
    }
    for (const id of snapshot.images) {
      const node = wrappers.get(id);
      if (node instanceof HTMLImageElement) node.__syncImage();
    }
    const resized = snapshot.resized.map((id) => wrappers.get(id)).filter(Boolean);
    if (scrolled.length) queueMicrotask(() => {
      for (const element of scrolled) {
        const target = element === documentNode.documentElement ? documentNode : element;
        dispatch(target, new Event("scroll", { bubbles: target === documentNode }));
      }
    });
    if (resized.length) queueMicrotask(() => {
      for (const observer of resizeObservers) {
        const entries = resized.filter((target) => observer.targets.has(target)).map((target) => ({
          target,
          contentRect: target.getBoundingClientRect(),
          borderBoxSize: [{ inlineSize: target.offsetWidth, blockSize: target.offsetHeight }],
          contentBoxSize: [{ inlineSize: target.clientWidth, blockSize: target.clientHeight }],
        }));
        if (entries.length) observer.callback(entries, observer);
      }
    });
  }

  const shellChannel = new MessageChannel();
  shellChannel.port1.addEventListener("message", (event) => {
    sendShellMessage(event.data);
  });
  shellChannel.port1.start();

  Object.assign(globalThis, {
    // MainWorker exposes the web platform on globalThis, but unlike a browser
    // it does not install the Window aliases expected by browser-targeted
    // modules emitted by browser-targeted Vite builds.
    window: windowObject,
    self: windowObject,
    parent: windowObject,
    top: windowObject,
    MouseEvent,
    WheelEvent,
    KeyboardEvent,
    Node,
    Text,
    Comment,
    Element,
    HTMLElement,
    HTMLImageElement,
    HTMLHeadElement,
    HTMLBodyElement,
    HTMLDivElement: HTMLElement,
    HTMLIFrameElement: HTMLElement,
    HTMLInputElement: HTMLElement,
    HTMLSelectElement: HTMLElement,
    HTMLTextAreaElement: HTMLElement,
    SVGElement: HTMLElement,
    MutationObserver,
    ResizeObserver,
    IntersectionObserver,
    PointerEvent,
    FocusEvent,
    InputEvent,
    CompositionEvent,
    DocumentFragment,
    HTMLTemplateElement,
    HTMLTrackedSurfaceElement,
    Document,
    document: null,
    scroll(...args) { documentNode.documentElement?.scrollTo(...args); },
    scrollTo(...args) { documentNode.documentElement?.scrollTo(...args); },
    scrollBy(...args) { documentNode.documentElement?.scrollBy(...args); },
    getComputedStyle: (element) => new Proxy(Object.freeze({
      getPropertyValue: (name) =>
        ops.op_dom_computed_style(element.__id, String(name)) ?? element.style.getPropertyValue(name),
    }), {
      get: (target, property) => typeof property !== "string" || property in target
        ? Reflect.get(target, property)
        : target.getPropertyValue(camelToDash(property)),
    }),
    postMessage: shellChannel.port2.postMessage.bind(shellChannel.port2),
  });
  Object.defineProperties(globalThis, {
    innerWidth: { get: () => dimensions.width },
    innerHeight: { get: () => dimensions.height },
    devicePixelRatio: { get: () => deviceScaleFactor },
    scrollX: { get: () => documentNode?.documentElement?.scrollLeft ?? 0 },
    scrollY: { get: () => documentNode?.documentElement?.scrollTop ?? 0 },
    pageXOffset: { get: () => documentNode?.documentElement?.scrollLeft ?? 0 },
    pageYOffset: { get: () => documentNode?.documentElement?.scrollTop ?? 0 },
  });
  const shellLocation = Object.create(globalThis.location, {
    reload: { value: function reload() { ops.op_shell_reload(); }, enumerable: true },
  });
  Object.defineProperty(globalThis, "location", {
    ...Object.getOwnPropertyDescriptor(globalThis, "location"),
    get: () => shellLocation,
    configurable: false,
  });
  const shellNavigator = Object.freeze(Object.create(globalThis.navigator, {
    clearImageMipChains: { value: function clearImageMipChains() { ops.op_dom_clear_image_mip_chains(); }, enumerable: true },
  }));
  Object.defineProperty(globalThis, "navigator", { value: shellNavigator, configurable: false });
  Object.defineProperty(globalThis, "CSS", { value: Object.freeze({
    escape(value) { return String(value).replace(/[^a-zA-Z0-9_-]/g, (character) => `\\${character}`); },
  }) });
  let nextAnimationFrame = 0;
  const animationFrames = new Map();
  globalThis.requestAnimationFrame = (callback) => {
    const id = ++nextAnimationFrame;
    animationFrames.set(id, setTimeout(() => {
      animationFrames.delete(id);
      callback(performance.now());
    }, 16));
    return id;
  };
  globalThis.cancelAnimationFrame = (id) => {
    const timer = animationFrames.get(id);
    if (timer !== undefined) clearTimeout(timer);
    animationFrames.delete(id);
  };

  globalThis.__polymerDom = Object.freeze({
    install(documentId, width, height, scaleFactor) {
      dimensions = { width, height };
      setDeviceScaleFactor(scaleFactor);
      documentNode = nodeFor(documentId);
      globalThis.document = documentNode;
      windowObject.document = documentNode;
    },
    syncLayout() {
      syncLayout(ops.op_dom_take_layout());
      runMicrotasks();
    },
    resize(width, height, scaleFactor) {
      dimensions = { width, height };
      setDeviceScaleFactor(scaleFactor);
      dispatch(windowObject, new Event("resize"));
    },
    dispatchDom(type, targetId, eventClass, init, bubbles, cancelable) {
      let target = nodeFor(targetId) || documentNode;
      if (type === "scroll" && target instanceof Element) {
        target.__notifiedScroll = [init.scrollLeft, init.scrollTop];
        if (target === documentNode.documentElement) {
          target = documentNode;
          bubbles = true;
        }
      }
      if (eventClass === "pointer" || eventClass === "mouse") {
        target = pointerCaptureTarget(init.pointerId) || target;
      }
      if (eventClass === "input" && target instanceof Element) {
        target.__value = init.data;
      }
      const constructors = { mouse: MouseEvent, pointer: PointerEvent, wheel: WheelEvent, keyboard: KeyboardEvent, focus: FocusEvent, input: InputEvent, composition: CompositionEvent };
      const Constructor = constructors[eventClass] || Event;
      dispatch(target, new Constructor(type, { bubbles, cancelable, ...init }));
      // Proc the microtask queue before propagating DOM effects.
      runMicrotasks();
      if (eventClass === "pointer" && (type === "pointerup" || type === "pointercancel")) {
        const pointerId = Number(init.pointerId) || 1;
        target.__capturedPointers?.delete(pointerId);
        if (pointerCaptures.get(pointerId) === target) pointerCaptures.delete(pointerId);
      }
    },
  });
})();
