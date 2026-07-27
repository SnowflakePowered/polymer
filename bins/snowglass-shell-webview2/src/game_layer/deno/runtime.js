(() => {
  "use strict";

  const { ops, webEvents } = globalThis.__snowglassRuntime;
  const {
    defineEventHandler,
    dispatch: dispatchNativeEvent,
    listenerCount,
  } = webEvents;
  delete globalThis.__snowglassRuntime;
  const nodes = new Map();
  let documentNode = null;
  let nextNodeId = 1;
  let eventResult = { handled: false, defaultPrevented: false };
  const proxyObjects = new Map([[0, globalThis]]);
  const proxyObjectHandles = new WeakMap([[globalThis, 0]]);
  const foregroundCallbacks = new Map();
  let nextProxyObject = 1;

  function proxyReference(value) {
    let handle = proxyObjectHandles.get(value);
    if (handle === undefined) {
      handle = nextProxyObject++;
      proxyObjectHandles.set(value, handle);
      proxyObjects.set(handle, value);
    }
    return {
      kind: "backgroundReference",
      handle,
    };
  }

  function encodeProxyValue(value, seen = new Set()) {
    if (value === undefined) return { kind: "undefined" };
    if (typeof value === "bigint") {
      return { kind: "bigint", value: value.toString() };
    }
    if (
      value === null ||
      typeof value === "string" ||
      typeof value === "number" ||
      typeof value === "boolean"
    ) {
      return { kind: "value", value };
    }
    if (typeof value === "function") return proxyReference(value);
    if (typeof value !== "object") {
      throw new TypeError(`Cannot proxy a ${typeof value} value`);
    }
    if (seen.has(value)) return proxyReference(value);

    const prototype = Object.getPrototypeOf(value);
    if (!Array.isArray(value) && prototype !== Object.prototype && prototype !== null) {
      return proxyReference(value);
    }
    seen.add(value);
    try {
      if (Array.isArray(value)) {
        return {
          kind: "array",
          value: value.map((item) => encodeProxyValue(item, seen)),
        };
      }
      return {
        kind: "object",
        value: Object.fromEntries(
          Object.entries(value).map(([key, item]) => [
            key,
            encodeProxyValue(item, seen),
          ]),
        ),
      };
    } finally {
      seen.delete(value);
    }
  }

  function decodeProxyValue(value) {
    switch (value?.kind) {
      case "undefined":
        return undefined;
      case "bigint":
        return BigInt(value.value);
      case "array":
        return value.value.map(decodeProxyValue);
      case "object":
        return Object.fromEntries(
          Object.entries(value.value).map(([key, item]) => [
            key,
            decodeProxyValue(item),
          ]),
        );
      case "backgroundReference": {
        let current = proxyObjects.get(value.handle);
        if (current === undefined) {
          throw new ReferenceError(`Unknown background object ${value.handle}`);
        }
        for (const property of value.path || []) current = current[property];
        return current;
      }
      case "foregroundCallback": {
        let callback = foregroundCallbacks.get(value.handle);
        if (!callback) {
          callback = (...args) => {
            ops.op_proxy_message({
              type: "callback",
              handle: value.handle,
              args: args.map((argument) => encodeProxyValue(argument)),
            });
          };
          foregroundCallbacks.set(value.handle, callback);
        }
        return callback;
      }
      case "value":
        return value.value;
      default:
        throw new TypeError("Invalid foreground proxy value");
    }
  }

  function proxyTarget(handle, path) {
    let target = proxyObjects.get(handle);
    if (target === undefined) {
      throw new ReferenceError(`Unknown background object ${handle}`);
    }
    for (const property of path) target = target[property];
    return target;
  }

  function proxyOwner(handle, path) {
    if (path.length === 0) {
      return { owner: undefined, property: undefined, value: proxyTarget(handle, path) };
    }
    const owner = proxyTarget(handle, path.slice(0, -1));
    const property = path.at(-1);
    return { owner, property, value: owner[property] };
  }

  function proxyError(error) {
    return {
      name: error?.name || "Error",
      message: error?.message || String(error),
      stack: error?.stack,
    };
  }

  function proxyRequest(request) {
    void (async () => {
      try {
        const path = Array.isArray(request.path) ? request.path.map(String) : [];
        let result;
        switch (request.operation) {
          case "get":
            result = proxyTarget(request.handle, path);
            break;
          case "apply": {
            const { owner, value } = proxyOwner(request.handle, path);
            if (typeof value !== "function") {
              throw new TypeError(`${path.join(".") || "remote value"} is not callable`);
            }
            result = Reflect.apply(
              value,
              owner,
              (request.args || []).map(decodeProxyValue),
            );
            break;
          }
          case "set": {
            const { owner, property } = proxyOwner(request.handle, path);
            if (owner === undefined || property === undefined) {
              throw new TypeError("Cannot replace a proxy root");
            }
            result = Reflect.set(owner, property, decodeProxyValue(request.value));
            break;
          }
          case "delete": {
            const { owner, property } = proxyOwner(request.handle, path);
            if (owner === undefined || property === undefined) {
              throw new TypeError("Cannot delete a proxy root");
            }
            result = Reflect.deleteProperty(owner, property);
            break;
          }
          default:
            throw new TypeError(`Unknown proxy operation '${request.operation}'`);
        }
        result = await result;
        ops.op_proxy_message({
          type: "response",
          id: request.id,
          ok: true,
          value: encodeProxyValue(result),
        });
      } catch (error) {
        ops.op_proxy_message({
          type: "response",
          id: request.id,
          ok: false,
          error: proxyError(error),
        });
      }
    })();
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

  // deno_runtime supplies Event, EventTarget, CustomEvent and MessageEvent.
  // MouseEvent is part of the browser DOM rather than deno_web, so the Blitz
  // bridge adds only that DOM-specific subclass.
  class MouseEvent extends Event {
    constructor(type, init = {}) {
      super(type, { bubbles: true, cancelable: true, ...init });
      this.clientX = Number(init.clientX) || 0;
      this.clientY = Number(init.clientY) || 0;
      this.screenX = Number(init.screenX) || this.clientX;
      this.screenY = Number(init.screenY) || this.clientY;
      this.button = Number(init.button) || 0;
      this.buttons = Number(init.buttons) || 0;
      this.ctrlKey = Boolean(init.ctrlKey);
      this.shiftKey = Boolean(init.shiftKey);
      this.altKey = Boolean(init.altKey);
      this.metaKey = Boolean(init.metaKey);
    }

    get x() {
      return this.clientX;
    }

    get y() {
      return this.clientY;
    }
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
      child.parentNode?.__removeLocalChild(child);
      this.__childIds.push(child.__id);
      child.__parentId = this.__id;
      ops.op_dom_append_child(this.__id, child.__id);
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

  class Element extends Node {
    constructor(snapshot) {
      super(snapshot);
      this.style = new CSSStyleDeclaration(this);
      this.classList = new DOMTokenList(this, "class");
      this.__attributes = {};
      this.__update(snapshot);
    }

    __update(snapshot) {
      super.__update(snapshot);
      this.localName = String(snapshot.tag || "").toLowerCase();
      this.tagName = this.localName.toUpperCase();
      this.nodeName = this.tagName;
      this.namespaceURI = "http://www.w3.org/1999/xhtml";
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
      const { x, y, width, height } = this.__rect;
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

    focus() {}

    blur() {}

    get innerHTML() {
      return this.textContent;
    }

    set innerHTML(value) {
      this.textContent = String(value ?? "");
    }
  }

  class HTMLElement extends Element {}
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
      return this.body;
    }

    get visibilityState() {
      return "visible";
    }

    get hidden() {
      return false;
    }

    createElement(tag) {
      const snapshot = {
        id: nextNodeId++,
        parent: null,
        children: [],
        kind: "element",
        tag: String(tag).toLowerCase(),
        attributes: {},
        rect: { x: 0, y: 0, width: 0, height: 0 },
      };
      const element = makeNode(snapshot);
      nodes.set(snapshot.id, element);
      ops.op_dom_create_element(snapshot.id, snapshot.tag);
      return element;
    }

    createElementNS(_namespace, tag) {
      return this.createElement(tag);
    }

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

  for (const type of ["mousedown", "mouseup", "mousemove", "click"]) {
    defineEventHandler(Element.prototype, type);
    defineEventHandler(Document.prototype, type);
    defineEventHandler(globalThis, type);
  }
  defineEventHandler(globalThis, "message");

  let dimensions = { width: 0, height: 0 };
  const windowObject = globalThis;

  class GameViewInstance {
    #listeners = new Map();
    #closed = false;

    constructor(id, requested) {
      this.id = id;
      this.requested = Object.freeze([...requested]);
      this.surfaces = Object.freeze([]);
    }

    on(eventName, callback) {
      if (typeof callback !== "function") throw new TypeError("event callback must be a function");
      const listeners = this.#listeners.get(String(eventName)) || new Set();
      listeners.add(callback);
      this.#listeners.set(String(eventName), listeners);
    }

    message(name, args) {
      if (!this.#closed) ops.op_game_message(this.id, String(name), args ?? null);
    }

    close() {
      if (this.#closed) return;
      ops.op_game_close(this.id);
      this.#closed = true;
    }

    __nativeEvent(eventName, payload) {
      if (eventName === "surfacesChanged") {
        const connected = new Map(
          Array.isArray(payload) ? payload.map((surface) => [surface.index, surface]) : [],
        );
        this.surfaces = Object.freeze(
          this.requested.filter((surface) => connected.has(surface.index)),
        );
        for (const surface of this.requested) {
          const native = connected.get(surface.index);
          if (!native) continue;
          surface.attach.dataset.connection = String(native.connection);
          surface.attach.dataset.index = String(native.index);
          surface.attach.dataset.preferredWidth = String(native.width);
          surface.attach.dataset.preferredHeight = String(native.height);
          surface.attach.style.setProperty("--preferred-width", `${native.width}px`);
          surface.attach.style.setProperty("--preferred-height", `${native.height}px`);
          surface.attach.style.setProperty(
            "--preferred-aspect-ratio",
            native.width && native.height ? `${native.width} / ${native.height}` : "auto",
          );
        }
      } else if (eventName === "closed") {
        this.#closed = true;
        this.surfaces = Object.freeze([]);
        gameViewManager.__release(this);
      }
      for (const callback of this.#listeners.get(eventName) || []) {
        try {
          callback(payload);
        } catch (error) {
          console.error(`GameView '${eventName}' handler failed`, error);
        }
      }
    }
  }

  class GameViewManager {
    #active = null;

    get activeInstance() {
      return this.#active;
    }

    launch(options) {
      if (this.#active) return null;
      try {
        if (!options || typeof options.command !== "string" || !options.command.trim()) {
          throw new TypeError("GameView launch requires a command");
        }
        if (!Array.isArray(options.surfaces) || options.surfaces.length === 0) {
          throw new TypeError("GameView launch requires at least one tracked surface");
        }
        const indices = new Set();
        for (const surface of options.surfaces) {
          if (!(surface.attach instanceof HTMLTrackedSurfaceElement)) {
            throw new TypeError(
              `surface ${surface.index} must attach to an HTMLTrackedSurfaceElement`,
            );
          }
          if (!surface.attach.isConnected) {
            throw new TypeError(`surface ${surface.index} attachment is not connected`);
          }
          if (!Number.isInteger(surface.index) || surface.index < 0 || surface.index >= 16) {
            throw new RangeError(`surface index ${surface.index} is outside 0..=15`);
          }
          if (indices.has(surface.index)) {
            throw new TypeError(`surface index ${surface.index} is duplicated`);
          }
          indices.add(surface.index);
        }
        const wire = {
          command: options.command,
          arguments: [...(options.arguments || [])],
          isolated: Boolean(options.isolated),
          hook: [...options.hook],
          surfaces: options.surfaces.map((surface) => ({
            index: surface.index,
            rotation: surface.rotation || 0,
            selector: surface.selector,
            attach: surface.attach.__id,
            webrtc: Boolean(surface.webrtc),
          })),
          gamepadMapping: (options.gamepadMapping || []).map((mapping) => ({
            ...mapping,
            mapping: { ...(mapping.mapping || {}) },
          })),
          webrtcBind: options.webrtcBind,
        };
        const id = ops.op_game_launch(wire);
        const instance = new GameViewInstance(id, options.surfaces);
        this.#active = instance;
        return instance;
      } catch (error) {
        console.error("GameView launch failed", error);
        return null;
      }
    }

    activateInput() {
      if (this.#active) ops.op_game_input(true);
    }

    deactivateInput() {
      ops.op_game_input(false);
    }

    __release(instance) {
      if (this.#active === instance) this.#active = null;
    }

    __nativeEvent(event) {
      if (this.#active?.id === String(event.instanceId)) {
        this.#active.__nativeEvent(event.eventName, event.payload);
      }
    }
  }

  const gameViewManager = new GameViewManager();

  function dispatch(target, event) {
    let handled = false;
    for (let current = target; current; current = current.parentNode) {
      if (listenerCount(current, event.type) > 0) {
        handled = true;
        break;
      }
    }
    dispatchNativeEvent(target, event);
    eventResult = { handled, defaultPrevented: event.defaultPrevented };
    ops.op_event_result(eventResult.handled, eventResult.defaultPrevented);
  }

  function makeNode(snapshot) {
    if (snapshot.kind === "document") return new Document(snapshot);
    if (snapshot.kind === "text") return new Text(snapshot);
    if (snapshot.kind === "comment") return new Comment(snapshot);
    if (snapshot.tag === "tracked-surface") return new HTMLTrackedSurfaceElement(snapshot);
    return new HTMLElement(snapshot);
  }

  function sync(snapshot) {
    dimensions = { width: snapshot.width, height: snapshot.height };
    for (const item of snapshot.nodes) {
      nextNodeId = Math.max(nextNodeId, item.id + 1);
      const current = nodes.get(item.id);
      if (current) current.__update(item);
      else nodes.set(item.id, makeNode(item));
    }
    for (const id of [...nodes.keys()]) {
      if (!snapshot.nodes.some((item) => item.id === id)) nodes.delete(id);
    }
    documentNode = nodes.get(snapshot.document);
  }

  const foregroundChannel = new MessageChannel();
  foregroundChannel.port1.addEventListener("message", (event) => {
    ops.op_post_message(event.data);
  });
  foregroundChannel.port1.start();

  Object.assign(globalThis, {
    // MainWorker exposes the web platform on globalThis, but unlike a browser
    // it does not install the Window aliases expected by browser-targeted
    // modules such as Vite's React-refresh runtime.
    window: windowObject,
    self: windowObject,
    parent: windowObject,
    top: windowObject,
    MouseEvent,
    Node,
    Text,
    Comment,
    Element,
    HTMLElement,
    HTMLDivElement: HTMLElement,
    HTMLIFrameElement: HTMLElement,
    HTMLInputElement: HTMLElement,
    HTMLSelectElement: HTMLElement,
    HTMLTextAreaElement: HTMLElement,
    SVGElement: HTMLElement,
    DocumentFragment: Node,
    HTMLTrackedSurfaceElement,
    Document,
    document: null,
    getComputedStyle: (element) => element.style,
    postMessage: foregroundChannel.port2.postMessage.bind(foregroundChannel.port2),
    gameView: gameViewManager,
    snowglassGameView: gameViewManager,
  });

  Object.defineProperties(globalThis, {
    innerWidth: { get: () => dimensions.width },
    innerHeight: { get: () => dimensions.height },
  });

  globalThis.__snowglassDom = Object.freeze({
    install(snapshot) {
      sync(snapshot);
      globalThis.document = documentNode;
      windowObject.document = documentNode;
    },
    sync,
    dispatchMouse(type, targetId, init) {
      const target = nodes.get(targetId) || documentNode;
      dispatch(target, new MouseEvent(type, init));
    },
    dispatchMessage(data) {
      dispatch(windowObject, new MessageEvent("message", { data }));
    },
    dispatchGameViewEvent(event) {
      gameViewManager.__nativeEvent(event);
    },
    proxyRequest,
  });
})();
