(() => {
  "use strict";

  const { ops, runMicrotasks, webEvents } = globalThis.__polymerRuntime;
  const registerPolymerSurface = globalThis.__polymerApi.registerSurface;
  const { defineEventHandler } = webEvents;
  const eventDispatch = globalThis.__polymerCreateEventDispatch(webEvents, ops);
  delete globalThis.__polymerCreateEventDispatch;
  const { MouseEvent, PointerEvent, WheelEvent, KeyboardEvent, FocusEvent, InputEvent, CompositionEvent, dispatch, dispatchNativeEvent } = eventDispatch;
  delete globalThis.__polymerRuntime;
  const nodes = new Map();
  const pointerCaptures = new Map();
  let documentNode = null;
  let nextNodeId = 1;
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

  class Node extends EventTarget {
    constructor(snapshot) {
      super();
      this.__update(snapshot);
    }

    __update(snapshot) {
      this.__id = snapshot.id;
      this.__kind = snapshot.kind;
      this.__parentId = snapshot.parent;
      this.__childIds = snapshot.children || [];
      this.__rect = snapshot.rect || { x: 0, y: 0, width: 0, height: 0 };
    }

    get parentNode() {
      return nodes.get(this.__parentId) || null;
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
      return this.__childIds.map((id) => nodes.get(id)).filter(Boolean);
    }

    get firstChild() {
      return this.childNodes[0] || null;
    }

    get lastChild() {
      return this.childNodes.at(-1) || null;
    }

    get nextSibling() {
      const siblings = this.parentNode?.childNodes || [];
      const index = siblings.indexOf(this);
      return index >= 0 ? siblings[index + 1] || null : null;
    }

    get previousSibling() {
      const siblings = this.parentNode?.childNodes || [];
      const index = siblings.indexOf(this);
      return index > 0 ? siblings[index - 1] : null;
    }

    appendChild(child) {
      if (!(child instanceof Node)) throw new TypeError("child must be a Node");
      if (child instanceof DocumentFragment) {
        for (const item of [...child.childNodes]) this.appendChild(item);
        return child;
      }
      child.parentNode?.__removeLocalChild(child);
      this.__childIds.push(child.__id);
      child.__parentId = this.__id;
      ops.op_dom_append_child(this.__id, child.__id);
      notifyMutation(this, { type: "childList", addedNodes: [child], removedNodes: [] });
      return child;
    }

    insertBefore(child, before) {
      if (before === null) return this.appendChild(child);
      if (!(child instanceof Node) || !(before instanceof Node)) {
        throw new TypeError("insertBefore expects Nodes");
      }
      const index = this.__childIds.indexOf(before.__id);
      if (index < 0) throw new DOMException("The reference node is not a child", "NotFoundError");
      child.parentNode?.__removeLocalChild(child);
      this.__childIds.splice(index, 0, child.__id);
      child.__parentId = this.__id;
      ops.op_dom_insert_before(this.__id, child.__id, before.__id);
      notifyMutation(this, { type: "childList", addedNodes: [child], removedNodes: [] });
      return child;
    }

    __removeLocalChild(child) {
      const index = this.__childIds.indexOf(child.__id);
      if (index >= 0) this.__childIds.splice(index, 1);
      child.__parentId = null;
    }

    removeChild(child) {
      if (!(child instanceof Node) || child.__parentId !== this.__id) {
        throw new DOMException("The node is not a child", "NotFoundError");
      }
      this.__removeLocalChild(child);
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
      for (const item of items) {
        this.appendChild(
          item instanceof Node ? item : documentNode.createTextNode(String(item)),
        );
      }
    }

    prepend(...items) {
      for (const item of items.reverse()) {
        this.insertBefore(item instanceof Node ? item : documentNode.createTextNode(String(item)), this.firstChild);
      }
    }

    before(...items) {
      const parent = this.parentNode;
      if (!parent) return;
      for (const item of items) parent.insertBefore(item instanceof Node ? item : documentNode.createTextNode(String(item)), this);
    }

    after(...items) {
      const parent = this.parentNode;
      if (!parent) return;
      const reference = this.nextSibling;
      for (const item of items) parent.insertBefore(item instanceof Node ? item : documentNode.createTextNode(String(item)), reference);
    }

    replaceWith(...items) {
      this.before(...items);
      this.remove();
    }

    cloneNode(deep = false) {
      let clone;
      if (this instanceof HTMLTemplateElement) {
        clone = documentNode.createElement("template");
        if (deep) for (const child of this.content.childNodes) clone.content.appendChild(child.cloneNode(true));
      } else if (this instanceof Element) {
        clone = documentNode.createElementNS(this.namespaceURI, this.localName);
        for (const { name, value } of this.attributes) clone.setAttribute(name, value);
      } else if (this instanceof Text) clone = documentNode.createTextNode(this.data);
      else if (this instanceof Comment) clone = documentNode.createComment(this.data);
      else clone = documentNode.createDocumentFragment();
      if (deep && !(this instanceof HTMLTemplateElement)) {
        for (const child of this.childNodes) clone.appendChild(child.cloneNode(true));
      }
      return clone;
    }

    contains(other) {
      for (let current = other; current; current = current.parentNode) {
        if (current === this) return true;
      }
      return false;
    }

    get isConnected() {
      return documentNode?.contains(this) || false;
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
      if (this instanceof Text) return this.data;
      return this.childNodes.map((child) => child.textContent || "").join("");
    }

    set textContent(value) {
      if (this instanceof Text) {
        this.data = String(value ?? "");
        return;
      }
      for (const child of [...this.childNodes]) this.removeChild(child);
      const text = String(value ?? "");
      if (text) this.appendChild(documentNode.createTextNode(text));
    }
  }

  class DocumentFragment extends Node {
    constructor() {
      super({ id: nextNodeId++, parent: null, children: [], kind: "fragment", rect: { x: 0, y: 0, width: 0, height: 0 } });
    }
    get nodeType() { return 11; }
    get nodeName() { return "#document-fragment"; }
    querySelectorAll(selector) { return descendants(this).filter((element) => element.matches(selector)); }
    querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
  }

  function selectorParts(selector) {
    return String(selector).trim().split(/\s+/).filter(Boolean);
  }

  function matchesSimple(element, selector) {
    if (!(element instanceof Element)) return false;
    let rest = selector;
    const tag = rest.match(/^[a-zA-Z][\w-]*/)?.[0];
    if (tag) {
      if (element.localName !== tag.toLowerCase()) return false;
      rest = rest.slice(tag.length);
    }
    for (const match of rest.matchAll(/([.#])([\w-]+)|\[([\w:-]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\]]+)))?\]/g)) {
      if (match[1] === "#" && element.id !== match[2]) return false;
      if (match[1] === "." && !element.classList.contains(match[2])) return false;
      if (match[3]) {
        if (!element.hasAttribute(match[3])) return false;
        const expected = match[4] ?? match[5] ?? match[6];
        if (expected !== undefined && element.getAttribute(match[3]) !== expected.trim()) return false;
      }
    }
    return true;
  }

  function matchesSelector(element, selector) {
    return String(selector).split(",").some((candidate) => {
      const parts = selectorParts(candidate);
      let current = element;
      for (let index = parts.length - 1; index >= 0; index -= 1) {
        while (current && !matchesSimple(current, parts[index])) current = current.parentElement;
        if (!current) return false;
        if (index > 0) current = current.parentElement;
      }
      return parts.length > 0;
    });
  }

  function descendants(root) {
    const result = [];
    const visit = (node) => {
      for (const child of node?.childNodes || []) {
        if (child instanceof Element) result.push(child);
        visit(child);
      }
    };
    visit(root);
    return result;
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
    const rect = element.__rect;
    let x = rect.x;
    let y = rect.y;
    // Blitz snapshots contain layout coordinates. CSS transforms do not alter
    // layout, but browsers include the element's and its ancestors' transforms
    // in getBoundingClientRect(). Keep this calculation synchronous so a drag
    // state machine can write its transform and read the new box in one event.
    for (let current = element; current instanceof Element; current = current.parentElement) {
      const translation = inlineTranslation(current);
      x += translation.x;
      y += translation.y;
    }
    return { x, y, width: rect.width, height: rect.height };
  }

  function pointerCaptureTarget(pointerId) {
    const id = Number(pointerId) || 1;
    return pointerCaptures.get(id) || null;
  }

  class Element extends Node {
    constructor(snapshot) {
      super(snapshot);
      this.style = new CSSStyleDeclaration(this);
      this.classList = new DOMTokenList(this, "class");
      this.__capturedPointers = new Set();
      this.__attributes = {};
      this.__update(snapshot);
    }

    __update(snapshot) {
      super.__update(snapshot);
      this.localName = String(snapshot.tag || "").toLowerCase();
      this.tagName = this.localName.toUpperCase();
      this.nodeName = this.tagName;
      this.namespaceURI = "http://www.w3.org/1999/xhtml";
      if (snapshot.namespace) this.namespaceURI = snapshot.namespace;
      this.__attributes = { ...(snapshot.attributes || {}) };
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
      return Object.entries(this.__attributes).map(([name, value]) => ({ name, value }));
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
        ownKeys() {
          return Object.keys(element.__attributes)
            .filter((name) => name.startsWith("data-"))
            .map((name) => dashToCamel(name.slice(5)));
        },
      });
    }

    hasAttribute(name) {
      return Object.hasOwn(this.__attributes, String(name).toLowerCase());
    }

    getAttribute(name) {
      name = String(name).toLowerCase();
      return this.hasAttribute(name) ? this.__attributes[name] : null;
    }

    setAttribute(name, value) {
      name = String(name).toLowerCase();
      value = String(value);
      this.__attributes[name] = value;
      ops.op_dom_set_attribute(this.__id, name, value);
      notifyMutation(this, { type: "attributes", attributeName: name });
    }

    getAttributeNS(_namespace, name) {
      return this.getAttribute(name);
    }

    setAttributeNS(_namespace, name, value) {
      this.setAttribute(name, value);
    }

    removeAttribute(name) {
      name = String(name).toLowerCase();
      if (!this.hasAttribute(name)) return;
      delete this.__attributes[name];
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
      return matchesSelector(this, selector);
    }

    closest(selector) {
      for (let current = this; current; current = current.parentElement) {
        if (current.matches(selector)) return current;
      }
      return null;
    }

    querySelectorAll(selector) {
      return descendants(this).filter((element) => element.matches(selector));
    }

    querySelector(selector) {
      return this.querySelectorAll(selector)[0] || null;
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
      return this.__rect.x;
    }

    get offsetTop() {
      return this.__rect.y;
    }

    get offsetWidth() {
      return this.__rect.width;
    }

    get offsetHeight() {
      return this.__rect.height;
    }

    get clientWidth() {
      return this.__rect.width;
    }

    get clientHeight() {
      return this.__rect.height;
    }

    get scrollLeft() {
      return 0;
    }

    get scrollTop() {
      return 0;
    }

    get scrollWidth() { return this.__rect.width; }
    get scrollHeight() { return this.__rect.height; }
    scrollTo() {}
    scrollBy() {}

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
    get value() { return this.getAttribute("value") || ""; }
    set value(value) { this.setAttribute("value", value); }
    get checked() { return this.hasAttribute("checked"); }
    set checked(value) { this.toggleAttribute("checked", Boolean(value)); }
    get disabled() { return this.hasAttribute("disabled"); }
    set disabled(value) { this.toggleAttribute("disabled", Boolean(value)); }
    get readOnly() { return this.hasAttribute("readonly"); }
    set readOnly(value) { this.toggleAttribute("readonly", Boolean(value)); }
    get required() { return this.hasAttribute("required"); }
    set required(value) { this.toggleAttribute("required", Boolean(value)); }

    focus() {
      if (activeElement === this) return;
      const previous = activeElement;
      activeElement = this;
      ops.op_dom_focus(this.__id);
      if (previous) { dispatch(previous, new Event("blur")); dispatch(previous, new Event("focusout", { bubbles: true })); }
      dispatch(this, new Event("focus")); dispatch(this, new Event("focusin", { bubbles: true }));
    }

    blur() { if (activeElement === this) documentNode?.body?.focus(); }

    get innerHTML() {
      return this.textContent;
    }

    set innerHTML(value) {
      this.textContent = String(value ?? "");
    }
  }

  class HTMLElement extends Element {}
  class HTMLImageElement extends HTMLElement {
    get naturalWidth() { return this.__imageSize?.[0] || 0; }
    get naturalHeight() { return this.__imageSize?.[1] || 0; }
    get complete() { return this.naturalWidth > 0 || !this.getAttribute("src"); }
    __update(snapshot) {
      super.__update(snapshot);
      this.__syncImage(snapshot.imageSize);
    }
    __syncImage(size = [0, 0]) {
      this.__imageSize = size;
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
    constructor(snapshot) {
      super(snapshot);
      this.content = new DocumentFragment();
    }
    get innerHTML() { return this.content.textContent; }
    set innerHTML(value) {
      for (const child of [...this.content.childNodes]) this.content.removeChild(child);
      for (const item of ops.op_dom_parse_fragment(String(value ?? ""))) {
        this.content.appendChild(createFragmentNode(item));
      }
    }
  }
  class Text extends Node {
    __update(snapshot) {
      super.__update(snapshot);
      this.__text = String(snapshot.text || "");
      this.nodeName = "#text";
    }

    get nodeType() {
      return 3;
    }

    get data() {
      return this.__text;
    }

    set data(value) {
      this.__text = String(value);
      ops.op_dom_set_text(this.__id, this.__text);
      notifyMutation(this, { type: "characterData" });
    }

    get nodeValue() {
      return this.data;
    }

    set nodeValue(value) {
      this.data = value ?? "";
    }

    get length() {
      return this.__text.length;
    }
  }

  class Comment extends Node {
    get nodeType() {
      return 8;
    }

    get nodeName() {
      return "#comment";
    }

    get data() {
      return "";
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
      return activeElement || this.body;
    }

    get visibilityState() {
      return "visible";
    }

    get hidden() {
      return false;
    }

    createElement(tag) {
      return this.createElementNS("http://www.w3.org/1999/xhtml", tag);
    }

    createElementNS(namespace, tag) {
      const snapshot = {
        id: nextNodeId++,
        parent: null,
        children: [],
        kind: "element",
        tag: String(tag).toLowerCase(),
        namespace: namespace || "http://www.w3.org/1999/xhtml",
        attributes: {},
        rect: { x: 0, y: 0, width: 0, height: 0 },
      };
      const element = snapshot.tag === "template" ? new HTMLTemplateElement(snapshot) : makeNode(snapshot);
      nodes.set(snapshot.id, element);
      ops.op_dom_create_element(snapshot.id, snapshot.tag, snapshot.namespace);
      return element;
    }

    createDocumentFragment() { return new DocumentFragment(); }

    importNode(node, deep = false) { return node.cloneNode(deep); }

    createTextNode(text) {
      const snapshot = {
        id: nextNodeId++,
        parent: null,
        children: [],
        kind: "text",
        text: String(text),
        attributes: {},
        rect: { x: 0, y: 0, width: 0, height: 0 },
      };
      const node = new Text(snapshot);
      nodes.set(snapshot.id, node);
      ops.op_dom_create_text(snapshot.id, snapshot.text);
      return node;
    }

    createComment() {
      const snapshot = {
        id: nextNodeId++,
        parent: null,
        children: [],
        kind: "comment",
        attributes: {},
        rect: { x: 0, y: 0, width: 0, height: 0 },
      };
      const node = new Comment(snapshot);
      nodes.set(snapshot.id, node);
      ops.op_dom_create_comment(snapshot.id);
      return node;
    }

    getElementById(id) {
      return descendants(this).find((element) => element.id === String(id)) || null;
    }

    querySelectorAll(selector) {
      const root = this.documentElement;
      if (!root) return [];
      return [root, ...descendants(root)].filter((element) => element.matches(selector));
    }

    querySelector(selector) {
      return this.querySelectorAll(selector)[0] || null;
    }

    getElementsByTagName(tag) {
      return this.querySelectorAll(String(tag));
    }
  }

  function createFragmentNode(item) {
    let node;
    if (item.kind === "element") {
      node = documentNode.createElementNS(item.namespace, item.tag);
      for (const [name, value] of Object.entries(item.attributes || {})) node.setAttribute(name, value);
    } else if (item.kind === "text") node = documentNode.createTextNode(item.text || "");
    else node = documentNode.createComment(item.text || "");
    for (const child of item.children || []) node.appendChild(createFragmentNode(child));
    return node;
  }

  for (const type of ["mousedown", "mouseup", "mousemove", "click", "keydown", "keyup"]) {
    defineEventHandler(Element.prototype, type);
    defineEventHandler(Document.prototype, type);
    defineEventHandler(globalThis, type);
  }
  defineEventHandler(globalThis, "message");

  let dimensions = { width: 0, height: 0 };
  let deviceScaleFactor = 1;
  let activeElement = null;
  const windowObject = globalThis;

  function setDeviceScaleFactor(value) {
    deviceScaleFactor = Number.isFinite(value) && value > 0 ? value : 1;
  }

  function makeNode(snapshot) {
    if (snapshot.kind === "document") return new Document(snapshot);
    if (snapshot.kind === "text") return new Text(snapshot);
    if (snapshot.kind === "comment") return new Comment(snapshot);
    if (snapshot.tag === "tracked-surface") {
      const surface = new HTMLTrackedSurfaceElement(snapshot);
      registerPolymerSurface(surface, snapshot.id);
      return surface;
    }
    if (snapshot.tag === "img") return new HTMLImageElement(snapshot);
    if (snapshot.tag === "head") return new HTMLHeadElement(snapshot);
    if (snapshot.tag === "body") return new HTMLBodyElement(snapshot);
    return new HTMLElement(snapshot);
  }

  function sync(snapshot) {
    dimensions = { width: snapshot.width, height: snapshot.height };
    setDeviceScaleFactor(snapshot.devicePixelRatio);
    const resized = [];
    for (const item of snapshot.nodes) {
      nextNodeId = Math.max(nextNodeId, item.id + 1);
      const current = nodes.get(item.id);
      if (current) {
        const old = current.__rect;
        current.__update(item);
        if (!old || old.width !== item.rect.width || old.height !== item.rect.height) resized.push(current);
      }
      else nodes.set(item.id, makeNode(item));
    }
    for (const id of [...nodes.keys()]) {
      if (!snapshot.nodes.some((item) => item.id === id)) nodes.delete(id);
    }
    documentNode = nodes.get(snapshot.document);
    activeElement = nodes.get(snapshot.activeElement) || (activeElement?.isConnected ? activeElement : documentNode?.body || null);
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

  function syncLayout(snapshot) {
    dimensions = { width: snapshot.width, height: snapshot.height };
    setDeviceScaleFactor(snapshot.devicePixelRatio);
    const resized = [];
    for (const item of snapshot.nodes) {
      const current = nodes.get(item.id);
      if (!current) continue;
      const old = current.__rect;
      current.__rect = item.rect;
      if (current instanceof HTMLImageElement) current.__syncImage(item.imageSize);
      if (!old || old.width !== item.rect.width || old.height !== item.rect.height) resized.push(current);
    }
    activeElement = nodes.get(snapshot.activeElement) || (activeElement?.isConnected ? activeElement : documentNode?.body || null);
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
    // Memory routing still uses the browser scroll-restoration hooks. The
    // shell viewport is fixed and element scrolling is owned by Blitz, so the
    // window-level operation is intentionally a stable no-op.
    scrollTo() {},
    scrollBy() {},
    getComputedStyle: (element) => Object.freeze({
      getPropertyValue: (name) => element.style.getPropertyValue(name),
      get display() { return element.style.display || "block"; },
      get visibility() { return element.style.visibility || "visible"; },
      get position() { return element.style.position || "static"; },
      get pointerEvents() { return element.style.pointerEvents || "auto"; },
      get opacity() { return element.style.opacity || "1"; },
    }),
    postMessage: shellChannel.port2.postMessage.bind(shellChannel.port2),
  });
  Object.defineProperties(globalThis, {
    innerWidth: { get: () => dimensions.width },
    innerHeight: { get: () => dimensions.height },
    devicePixelRatio: { get: () => deviceScaleFactor },
    scrollX: { get: () => 0 },
    scrollY: { get: () => 0 },
    pageXOffset: { get: () => 0 },
    pageYOffset: { get: () => 0 },
  });
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
    install(snapshot) {
      sync(snapshot);
      globalThis.document = documentNode;
      windowObject.document = documentNode;
    },
    sync,
    syncLayout() {
      syncLayout(ops.op_dom_take_layout());
    },
    resize(width, height, scaleFactor) {
      dimensions = { width, height };
      setDeviceScaleFactor(scaleFactor);
      dispatch(windowObject, new Event("resize"));
    },
    dispatchDom(type, targetId, eventClass, init, bubbles, cancelable) {
      let target = nodes.get(targetId) || documentNode;
      if (eventClass === "pointer" || eventClass === "mouse") {
        target = pointerCaptureTarget(init.pointerId) || target;
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
