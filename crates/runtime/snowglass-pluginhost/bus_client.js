// Shared structured-clone/FIFO client. WebView2 installs the allocation after
// navigation; calls made before that point remain queued as Promises.
(() => {
  const MAGIC = 0x31424753; // SGB1
  const CODEC_MAGIC = 0x31434753; // SGC1
  const VERSION = 1;
  const HEADER = 64;
  const LANES = 4;
  const [F_TO_N, N_TO_F] = globalThis.__snowglassBusLanes ?? [0, 1];
  const INVOKE = 1;
  const RESOLVE = 2;
  const REJECT = 3;
  const PLUGIN = 1;
  const BRIDGE_MESSAGE = 10;
  const BRIDGE_PROXY = 11;
  const SHELL_COMMAND = 12;
  const FOREGROUND = 2;
  const BACKGROUND = 3;
  const NATIVE = 4;
  const encoder = new TextEncoder();
  const decoder = new TextDecoder("utf-8", { fatal: true });
  let buffer = null;
  let generation = 0;
  let nextRequest = 1n;
  const waiting = new Map();
  const listeners = new Map();
  const outgoing = [];

  class Writer {
    constructor() { this.bytes = []; }
    u8(value) { this.bytes.push(value & 255); }
    u16(value) { this.u8(value); this.u8(value >>> 8); }
    u32(value) {
      this.u8(value); this.u8(value >>> 8); this.u8(value >>> 16); this.u8(value >>> 24);
    }
    u64(value) { this.u32(Number(value & 0xffffffffn)); this.u32(Number(value >> 32n)); }
    f64(value) {
      const bytes = new Uint8Array(8);
      new DataView(bytes.buffer).setFloat64(0, value, true);
      this.raw(bytes);
    }
    raw(bytes) { for (const byte of bytes) this.u8(byte); }
    blob(bytes) { this.u32(bytes.byteLength); this.raw(bytes); }
    string(value) { this.blob(encoder.encode(value)); }
    finish() { return Uint8Array.from(this.bytes); }
  }

  class Reader {
    constructor(bytes) { this.bytes = bytes; this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength); this.at = 0; }
    take(length) {
      if (!Number.isSafeInteger(length) || length < 0 || this.at + length > this.bytes.byteLength) throw new TypeError("malformed shared-bus payload");
      const result = this.bytes.subarray(this.at, this.at + length); this.at += length; return result;
    }
    u8() { return this.take(1)[0]; }
    u16() { const at = this.at; this.take(2); return this.view.getUint16(at, true); }
    u32() { const at = this.at; this.take(4); return this.view.getUint32(at, true); }
    u64() { return BigInt(this.u32()) | (BigInt(this.u32()) << 32n); }
    f64() { const at = this.at; this.take(8); return this.view.getFloat64(at, true); }
    blob() { return this.take(this.u32()); }
    string() { return decoder.decode(this.blob()); }
    done() { return this.at === this.bytes.byteLength; }
  }

  const typedKind = (value) => {
    if (value instanceof Int8Array) return 1;
    if (value instanceof Uint8Array) return 2;
    if (value instanceof Uint8ClampedArray) return 3;
    if (value instanceof Int16Array) return 4;
    if (value instanceof Uint16Array) return 5;
    if (value instanceof Int32Array) return 6;
    if (value instanceof Uint32Array) return 7;
    if (value instanceof Float32Array) return 8;
    if (value instanceof Float64Array) return 9;
    if (typeof BigInt64Array !== "undefined" && value instanceof BigInt64Array) return 10;
    if (typeof BigUint64Array !== "undefined" && value instanceof BigUint64Array) return 11;
    if (value instanceof DataView) return 12;
    return 0;
  };

  function encodeStructured(root) {
    const nodes = [];
    const seen = new Map();
    const value = (input) => {
      if (input === undefined) return [0];
      if (input === null) return [1];
      if (input === false) return [2];
      if (input === true) return [3];
      if (typeof input === "number") return [4, input];
      if (typeof input === "bigint") return [5, String(input)];
      if (typeof input === "string") return [6, input];
      if ((typeof input !== "object" && typeof input !== "function") || typeof input === "function") {
        throw new DOMException(`unsupported structured-clone value: ${typeof input}`, "DataCloneError");
      }
      if (seen.has(input)) return [7, seen.get(input)];
      if (input instanceof Promise || input instanceof WeakMap || input instanceof WeakSet) {
        throw new DOMException("unsupported structured-clone object", "DataCloneError");
      }
      const index = nodes.length;
      seen.set(input, index);
      nodes.push(null);
      if (Array.isArray(input)) nodes[index] = [1, input.map(value)];
      else if (input instanceof Date) nodes[index] = [3, input.getTime()];
      else if (input instanceof Map) nodes[index] = [4, [...input].map(([key, item]) => [value(key), value(item)])];
      else if (input instanceof Set) nodes[index] = [5, [...input].map(value)];
      else if (input instanceof Error) nodes[index] = [6, input.name, input.message, input.stack, "cause" in input ? value(input.cause) : null];
      else if (input instanceof ArrayBuffer) nodes[index] = [7, new Uint8Array(input)];
      else if (ArrayBuffer.isView(input)) {
        const kind = typedKind(input);
        if (!kind) throw new DOMException("unsupported typed array", "DataCloneError");
        nodes[index] = [8, kind, new Uint8Array(input.buffer, input.byteOffset, input.byteLength)];
      } else {
        const proto = Object.getPrototypeOf(input);
        if (proto !== Object.prototype && proto !== null) throw new DOMException("only plain objects can cross plugin isolates", "DataCloneError");
        nodes[index] = [2, Object.keys(input).map((key) => [key, value(input[key])])];
      }
      return [7, index];
    };
    const rootValue = value(root);
    const writer = new Writer();
    writer.u32(CODEC_MAGIC); writer.u16(VERSION); writer.u32(nodes.length);
    const writeValue = (wire) => {
      writer.u8(wire[0]);
      if (wire[0] === 4) writer.f64(wire[1]);
      else if (wire[0] === 5 || wire[0] === 6) writer.string(wire[1]);
      else if (wire[0] === 7) writer.u32(wire[1]);
    };
    writeValue(rootValue);
    for (const node of nodes) {
      writer.u8(node[0]);
      if (node[0] === 1 || node[0] === 5) { writer.u32(node[1].length); node[1].forEach(writeValue); }
      else if (node[0] === 2) { writer.u32(node[1].length); for (const [key, item] of node[1]) { writer.string(key); writeValue(item); } }
      else if (node[0] === 3) writer.f64(node[1]);
      else if (node[0] === 4) { writer.u32(node[1].length); for (const [key, item] of node[1]) { writeValue(key); writeValue(item); } }
      else if (node[0] === 6) {
        writer.string(node[1]); writer.string(node[2]);
        writer.u8(node[3] == null ? 0 : 1); if (node[3] != null) writer.string(node[3]);
        writer.u8(node[4] == null ? 0 : 1); if (node[4] != null) writeValue(node[4]);
      } else if (node[0] === 7) writer.blob(node[1]);
      else if (node[0] === 8) { writer.u8(node[1]); writer.blob(node[2]); }
    }
    return writer.finish();
  }

  function decodeStructured(bytes) {
    const reader = new Reader(bytes);
    if (reader.u32() !== CODEC_MAGIC || reader.u16() !== VERSION) throw new TypeError("unsupported shared-bus codec");
    const count = reader.u32();
    if (count > bytes.byteLength) throw new TypeError("invalid structured-clone node count");
    const readValue = () => {
      const tag = reader.u8();
      if (tag <= 3) return [tag];
      if (tag === 4) return [tag, reader.f64()];
      if (tag === 5 || tag === 6) return [tag, reader.string()];
      if (tag === 7) return [tag, reader.u32()];
      throw new TypeError("invalid structured-clone tag");
    };
    const root = readValue();
    const nodes = [];
    for (let index = 0; index < count; index++) {
      const tag = reader.u8();
      if (tag === 1 || tag === 5) { const length = reader.u32(); const items = []; for (let n = 0; n < length; n++) items.push(readValue()); nodes.push([tag, items]); }
      else if (tag === 2) { const length = reader.u32(); const entries = []; for (let n = 0; n < length; n++) entries.push([reader.string(), readValue()]); nodes.push([tag, entries]); }
      else if (tag === 3) nodes.push([tag, reader.f64()]);
      else if (tag === 4) { const length = reader.u32(); const entries = []; for (let n = 0; n < length; n++) entries.push([readValue(), readValue()]); nodes.push([tag, entries]); }
      else if (tag === 6) { const name = reader.string(); const message = reader.string(); const stack = reader.u8() ? reader.string() : null; const cause = reader.u8() ? readValue() : null; nodes.push([tag, name, message, stack, cause]); }
      else if (tag === 7) nodes.push([tag, new Uint8Array(reader.blob())]);
      else if (tag === 8) nodes.push([tag, reader.u8(), new Uint8Array(reader.blob())]);
      else throw new TypeError("invalid structured-clone node");
    }
    if (!reader.done()) throw new TypeError("trailing structured-clone data");
    const objects = nodes.map((node) => {
      if (node[0] === 1) return [];
      if (node[0] === 2) return {};
      if (node[0] === 3) return new Date(node[1]);
      if (node[0] === 4) return new Map();
      if (node[0] === 5) return new Set();
      if (node[0] === 6) return new Error(node[2]);
      if (node[0] === 7) return node[1].slice().buffer;
      const data = node[2].slice().buffer;
      const constructors = [null, Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array, Float32Array, Float64Array, globalThis.BigInt64Array, globalThis.BigUint64Array, DataView];
      try { return new constructors[node[1]](data); } catch { throw new TypeError("invalid typed-array payload"); }
    });
    const materialize = (wire) => {
      if (wire[0] === 0) return undefined;
      if (wire[0] === 1) return null;
      if (wire[0] === 2) return false;
      if (wire[0] === 3) return true;
      if (wire[0] === 4 || wire[0] === 6) return wire[1];
      if (wire[0] === 5) return BigInt(wire[1]);
      if (wire[1] >= objects.length) throw new TypeError("dangling structured-clone reference");
      return objects[wire[1]];
    };
    nodes.forEach((node, index) => {
      const target = objects[index];
      if (node[0] === 1) node[1].forEach((item) => target.push(materialize(item)));
      else if (node[0] === 2) for (const [key, item] of node[1]) Object.defineProperty(target, key, { value: materialize(item), enumerable: true, writable: true, configurable: true });
      else if (node[0] === 4) for (const [key, item] of node[1]) target.set(materialize(key), materialize(item));
      else if (node[0] === 5) for (const item of node[1]) target.add(materialize(item));
      else if (node[0] === 6) { target.name = node[1]; if (node[3] != null) target.stack = node[3]; if (node[4] != null) target.cause = materialize(node[4]); }
    });
    return materialize(root);
  }

  if (!globalThis.__snowglassStructuredCodec) {
    Object.defineProperty(globalThis, "__snowglassStructuredCodec", {
      value: Object.freeze({ encode: encodeStructured, decode: decodeStructured }),
      configurable: false,
    });
  }
  if (!globalThis.chrome?.webview || globalThis.__snowglassSharedBus) return;

  const lane = (index) => {
    if (!buffer) return null;
    const first = new DataView(buffer, 0, HEADER);
    const capacity = first.getUint32(12, true);
    const base = index * (HEADER + capacity);
    const view = new DataView(buffer, base, HEADER);
    if (view.getUint32(0, true) !== MAGIC || view.getUint16(4, true) !== VERSION || view.getUint16(6, true) !== index || view.getUint32(12, true) !== capacity) throw new TypeError("corrupt shared-bus lane header");
    if (Atomics.load(new Uint32Array(buffer, base + 8, 1), 0) !== generation || Atomics.load(new Uint32Array(buffer, base + 32, 1), 0) !== 0) throw new TypeError("stale or faulted shared-bus lane");
    return { base, capacity, data: base + HEADER, read: new BigUint64Array(buffer, base + 16, 1), write: new BigUint64Array(buffer, base + 24, 1) };
  };

  const ringWrite = (info, cursor, bytes) => {
    const target = new Uint8Array(buffer, info.data, info.capacity);
    for (let index = 0; index < bytes.length; index++) target[(Number(cursor % BigInt(info.capacity)) + index) % info.capacity] = bytes[index];
  };
  const ringRead = (info, cursor, length) => {
    const source = new Uint8Array(buffer, info.data, info.capacity); const bytes = new Uint8Array(length);
    for (let index = 0; index < length; index++) bytes[index] = source[(Number(cursor % BigInt(info.capacity)) + index) % info.capacity];
    return bytes;
  };

  const encodeRecord = (request) => {
    const path = encoder.encode(request.path);
    const payload = encodeStructured(request.value);
    const writer = new Writer();
    writer.u16(request.kind); writer.u16(0); writer.u64(request.id); writer.u8(request.target);
    writer.u16(path.length); writer.u32(payload.length); writer.raw(path); writer.raw(payload);
    return writer.finish();
  };
  const alignedRecordLength = (bodyLength) => (bodyLength + 4 + 7) & ~7;

  const tryPush = (request) => {
    const info = lane(F_TO_N); if (!info) return false;
    const body = encodeRecord(request); const recordLength = alignedRecordLength(body.length);
    let write = Atomics.load(info.write, 0); const read = Atomics.load(info.read, 0);
    if (write < read || write - read > BigInt(info.capacity)) throw new TypeError("corrupt shared-bus cursors");
    if (BigInt(recordLength) > BigInt(info.capacity) - (write - read)) return false;
    const commit = new Uint32Array(buffer, info.data + Number(write % BigInt(info.capacity)), 1);
    Atomics.store(commit, 0, 0); ringWrite(info, write + 4n, body); Atomics.store(commit, 0, body.length); Atomics.store(info.write, 0, write + BigInt(recordLength));
    return true;
  };

  const decodeRecord = (body) => {
    const reader = new Reader(body); const kind = reader.u16(); reader.u16(); const id = reader.u64(); const target = reader.u8();
    const pathLength = reader.u16(); const payloadLength = reader.u32(); const path = decoder.decode(reader.take(pathLength)); const payload = reader.take(payloadLength);
    if (!reader.done()) throw new TypeError("invalid shared-bus response");
    return { kind, id, target, path, value: decodeStructured(payload) };
  };

  const dispatch = (record) => {
    if (record.target === PLUGIN && (record.kind === RESOLVE || record.kind === REJECT)) {
      const call = waiting.get(record.id);
      if (call) { waiting.delete(record.id); record.kind === RESOLVE ? call.resolve(record.value) : call.reject(record.value); }
      return;
    }
    const listener = listeners.get(`${record.target}:${record.kind}`);
    if (!listener) throw new TypeError(`unsupported shared-bus record ${record.target}:${record.kind}`);
    listener(record.value, record);
  };

  const drain = () => {
    if (!buffer) return 0;
    const info = lane(N_TO_F);
    let drained = 0;
    while (true) {
      let read = Atomics.load(info.read, 0); const write = Atomics.load(info.write, 0);
      if (write < read || write - read > BigInt(info.capacity)) throw new TypeError("corrupt shared-bus cursors");
      if (read === write) break;
      const commit = new Uint32Array(buffer, info.data + Number(read % BigInt(info.capacity)), 1);
      const length = Atomics.load(commit, 0);
      if (length === 0) break;
      const recordLength = alignedRecordLength(length);
      if (recordLength > info.capacity || BigInt(recordLength) > write - read) throw new TypeError("corrupt shared-bus record length");
      const record = decodeRecord(ringRead(info, read + 4n, length));
      Atomics.store(commit, 0, 0); Atomics.store(info.read, 0, read + BigInt(recordLength));
      dispatch(record);
      drained++;
    }
    return drained;
  };

  const flush = () => {
    if (!buffer) {
      if (outgoing.length) globalThis.ipc.postMessage("");
      return;
    }
    let written = 0;
    while (outgoing.length && tryPush(outgoing[0])) { outgoing.shift(); written++; }
    if (written || outgoing.length) globalThis.ipc.postMessage("");
  };

  const rejectAll = () => {
    const error = new DOMException("plugin invocation was cancelled by navigation", "AbortError");
    for (const call of waiting.values()) call.reject(error);
    waiting.clear(); outgoing.length = 0;
  };

  const synchronizeGeneration = () => {
    if (!buffer) return;
    const current = Atomics.load(new Uint32Array(buffer, 8, 1), 0);
    if (current === generation) return;
    rejectAll();
    generation = current;
  };

  chrome.webview.addEventListener("sharedbufferreceived", (event) => {
    rejectAll();
    if (buffer) chrome.webview.releaseBuffer(buffer);
    buffer = event.getBuffer();
    generation = Atomics.load(new Uint32Array(buffer, 8, 1), 0);
    flush(); drain();
  });
  chrome.webview.addEventListener("message", () => {
    synchronizeGeneration();
    const drained = drain();
    flush();
    // A full native response lane may have more records waiting outside the
    // allocation. Ring once after freeing space so it can resume production.
    if (drained) globalThis.ipc.postMessage("");
  });
  Object.defineProperty(globalThis, "__snowglassSharedBusNotify", {
    value() {
      synchronizeGeneration();
      const drained = drain();
      flush();
      if (drained) globalThis.ipc.postMessage("");
    },
  });
  addEventListener("pagehide", () => {
    rejectAll();
    if (buffer) chrome.webview.releaseBuffer(buffer);
    buffer = null;
  }, { once: true });

  const api = Object.freeze({
    invoke(plugin, path, args) {
      const id = nextRequest++;
      return new Promise((resolve, reject) => {
        waiting.set(id, { resolve, reject });
        outgoing.push({ id, kind: INVOKE, target: PLUGIN, path: [plugin, ...path].join("\0"), value: args });
        flush();
      });
    },
    send(kind, target, value, path = "") {
      if (!Number.isInteger(kind) || !Number.isInteger(target)) throw new TypeError("invalid shared-bus route");
      const id = nextRequest++;
      outgoing.push({ id, kind, target, path: String(path), value });
      flush();
      return id;
    },
    subscribe(kind, target, listener) {
      if (typeof listener !== "function") throw new TypeError("shared-bus listener must be callable");
      const key = `${target}:${kind}`;
      if (listeners.has(key)) throw new TypeError(`shared-bus route ${key} already has a listener`);
      listeners.set(key, listener);
      return () => { if (listeners.get(key) === listener) listeners.delete(key); };
    },
    routes: Object.freeze({ BRIDGE_MESSAGE, BRIDGE_PROXY, SHELL_COMMAND, FOREGROUND, BACKGROUND, NATIVE }),
  });
  Object.defineProperty(globalThis, "__snowglassSharedBus", { value: api });
  Object.defineProperty(globalThis, "__snowglassPluginTransport", { value: api, configurable: false });
})();
