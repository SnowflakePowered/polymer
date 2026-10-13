// The Deno extension crates register ops and internal scripts, but their
// browser globals are normally installed by deno_runtime. Install only the
// globals Polymer documents use, leaving WebCrypto, Node and the Deno
// namespace absent by construction.
import { core } from "ext:core/mod.js";
const { ops } = core;

const webidl = core.loadExtScript("ext:deno_webidl/00_webidl.js");
const event = core.loadExtScript("ext:deno_web/02_event.js");
const url = core.loadExtScript("ext:deno_web/00_url.js");
const timers = core.loadExtScript("ext:deno_web/02_timers.js");
const messagePort = core.loadExtScript("ext:deno_web/13_message_port.js");
const performance = core.loadExtScript("ext:deno_web/15_performance.js");
const consoleModule = core.loadExtScript("ext:deno_web/01_console.js");
const abortSignal = core.loadExtScript("ext:deno_web/03_abort_signal.js");
const file = core.loadExtScript("ext:deno_web/09_file.js");
const domException = core.loadExtScript("ext:deno_web/01_dom_exception.js");
const locationModule = core.loadExtScript("ext:deno_web/12_location.js");
const globalInterfaces = core.loadExtScript("ext:deno_web/04_global_interfaces.js");
const structuredCloneModule = core.loadExtScript("ext:deno_web/02_structured_clone.js");
locationModule.setLocationHref(ops.op_polymer_location());

const lazyScript = (specifier) => {
  let module;
  return () => module ??= core.loadExtScript(specifier);
};
const loadStreams = lazyScript("ext:deno_web/06_streams.js");
const loadHeaders = lazyScript("ext:deno_fetch/20_headers.js");
const loadFormData = lazyScript("ext:deno_fetch/21_formdata.js");
const loadRequest = lazyScript("ext:deno_fetch/23_request.js");
const loadResponse = lazyScript("ext:deno_fetch/23_response.js");
const loadFetch = lazyScript("ext:deno_fetch/26_fetch.js");
const loadEncoding = lazyScript("ext:deno_web/08_text_encoding.js");
const loadBase64 = lazyScript("ext:deno_web/05_base64.js");
const loadBroadcastChannel = lazyScript("ext:deno_web/01_broadcast_channel.js");
const loadUrlPattern = lazyScript("ext:deno_web/01_urlpattern.js");
const loadFileReader = lazyScript("ext:deno_web/10_filereader.js");
const loadCompression = lazyScript("ext:deno_web/14_compression.js");
const loadImageData = lazyScript("ext:deno_web/16_image_data.js");
const loadGeometry = lazyScript("ext:deno_web/17_geometry.js");
const loadCssStyleSheet = lazyScript("ext:deno_web/18_css_stylesheet.js");
const loadWebStorage = lazyScript("ext:polymer_webstorage/01_webstorage.js");
const loadLocks = core.createLazyLoader("ext:deno_web/locks.js");
const loadWebTransport = core.createLazyLoader("ext:deno_web/webtransport.js");
const loadWebSocket = core.createLazyLoader(
  "ext:deno_websocket/01_websocket.js",
);
const loadWebSocketStream = core.createLazyLoader(
  "ext:deno_websocket/02_websocketstream.js",
);
const navigator = Object.freeze(Object.create(null, {
  locks: { get: () => loadLocks().locks, enumerable: true },
  userAgent: { value: ops.op_polymer_user_agent(), enumerable: true },
  platform: { value: "Win32", enumerable: true },
  language: { value: "en-US", enumerable: true },
  languages: { value: Object.freeze(["en-US"]), enumerable: true },
  hardwareConcurrency: { value: 1, enumerable: true },
}));

const lazy = (load, property) => ({
  configurable: true,
  enumerable: false,
  get() {
    const value = load()[property];
    Object.defineProperty(globalThis, property, {
      configurable: true,
      enumerable: false,
      writable: true,
      value,
    });
    return value;
  },
});

// Window globals inherit EventTarget through Window.prototype in browsers.
// Using the same prototype chain also satisfies deno_web's Web IDL brand
// checks when its methods are called as global functions.
Object.setPrototypeOf(globalThis, globalInterfaces.Window.prototype);

Object.defineProperties(globalThis, {
  addEventListener: { value: event.EventTarget.prototype.addEventListener, configurable: true, writable: true },
  atob: lazy(loadBase64, "atob"),
  btoa: lazy(loadBase64, "btoa"),
  AbortController: { value: abortSignal.AbortController, configurable: true, writable: true },
  AbortSignal: { value: abortSignal.AbortSignal, configurable: true, writable: true },
  Blob: { value: file.Blob, configurable: true, writable: true },
  BroadcastChannel: lazy(loadBroadcastChannel, "BroadcastChannel"),
  ByteLengthQueuingStrategy: lazy(loadStreams, "ByteLengthQueuingStrategy"),
  CloseEvent: { value: event.CloseEvent, configurable: true, writable: true },
  CompressionStream: lazy(loadCompression, "CompressionStream"),
  CountQueuingStrategy: lazy(loadStreams, "CountQueuingStrategy"),
  CSSRule: lazy(loadCssStyleSheet, "CSSRule"),
  CSSStyleSheet: lazy(loadCssStyleSheet, "CSSStyleSheet"),
  CustomEvent: { value: event.CustomEvent, configurable: true, writable: true },
  DecompressionStream: lazy(loadCompression, "DecompressionStream"),
  DOMException: { value: domException.DOMException, configurable: true, writable: true },
  DOMMatrix: lazy(loadGeometry, "DOMMatrix"),
  DOMMatrixReadOnly: lazy(loadGeometry, "DOMMatrixReadOnly"),
  DOMPoint: lazy(loadGeometry, "DOMPoint"),
  DOMPointReadOnly: lazy(loadGeometry, "DOMPointReadOnly"),
  DOMQuad: lazy(loadGeometry, "DOMQuad"),
  DOMRect: lazy(loadGeometry, "DOMRect"),
  DOMRectReadOnly: lazy(loadGeometry, "DOMRectReadOnly"),
  ErrorEvent: { value: event.ErrorEvent, configurable: true, writable: true },
  Event: { value: event.Event, configurable: true, writable: true },
  EventTarget: { value: event.EventTarget, configurable: true, writable: true },
  File: { value: file.File, configurable: true, writable: true },
  FileReader: lazy(loadFileReader, "FileReader"),
  FormData: lazy(loadFormData, "FormData"),
  Headers: lazy(loadHeaders, "Headers"),
  ImageData: lazy(loadImageData, "ImageData"),
  Lock: lazy(loadLocks, "Lock"),
  LockManager: lazy(loadLocks, "LockManager"),
  MessageChannel: { value: messagePort.MessageChannel, configurable: true, writable: true },
  MessageEvent: { value: event.MessageEvent, configurable: true, writable: true },
  MessagePort: { value: messagePort.MessagePort, configurable: true, writable: true },
  Performance: { value: performance.Performance, configurable: true, writable: true },
  PerformanceEntry: { value: performance.PerformanceEntry, configurable: true, writable: true },
  PerformanceMark: { value: performance.PerformanceMark, configurable: true, writable: true },
  PerformanceMeasure: { value: performance.PerformanceMeasure, configurable: true, writable: true },
  PerformanceObserver: { value: performance.PerformanceObserver, configurable: true, writable: true },
  PerformanceObserverEntryList: { value: performance.PerformanceObserverEntryList, configurable: true, writable: true },
  ProgressEvent: { value: event.ProgressEvent, configurable: true, writable: true },
  PromiseRejectionEvent: { value: event.PromiseRejectionEvent, configurable: true, writable: true },
  QuotaExceededError: { value: domException.QuotaExceededError, configurable: true, writable: true },
  ReadableByteStreamController: lazy(loadStreams, "ReadableByteStreamController"),
  ReadableStream: lazy(loadStreams, "ReadableStream"),
  ReadableStreamBYOBReader: lazy(loadStreams, "ReadableStreamBYOBReader"),
  ReadableStreamBYOBRequest: lazy(loadStreams, "ReadableStreamBYOBRequest"),
  ReadableStreamDefaultController: lazy(loadStreams, "ReadableStreamDefaultController"),
  ReadableStreamDefaultReader: lazy(loadStreams, "ReadableStreamDefaultReader"),
  Request: lazy(loadRequest, "Request"),
  Response: lazy(loadResponse, "Response"),
  Storage: lazy(loadWebStorage, "Storage"),
  TextDecoder: lazy(loadEncoding, "TextDecoder"),
  TextDecoderStream: lazy(loadEncoding, "TextDecoderStream"),
  TextEncoder: lazy(loadEncoding, "TextEncoder"),
  TextEncoderStream: lazy(loadEncoding, "TextEncoderStream"),
  TransformStream: lazy(loadStreams, "TransformStream"),
  TransformStreamDefaultController: lazy(loadStreams, "TransformStreamDefaultController"),
  URL: { value: url.URL, configurable: true, writable: true },
  URLPattern: lazy(loadUrlPattern, "URLPattern"),
  URLSearchParams: { value: url.URLSearchParams, configurable: true, writable: true },
  WebSocket: lazy(loadWebSocket, "WebSocket"),
  WebSocketStream: lazy(loadWebSocketStream, "WebSocketStream"),
  WebTransport: lazy(loadWebTransport, "WebTransport"),
  WebTransportBidirectionalStream: lazy(loadWebTransport, "WebTransportBidirectionalStream"),
  WebTransportDatagramDuplexStream: lazy(loadWebTransport, "WebTransportDatagramDuplexStream"),
  WebTransportError: lazy(loadWebTransport, "WebTransportError"),
  WebTransportReceiveStream: lazy(loadWebTransport, "WebTransportReceiveStream"),
  WebTransportSendGroup: lazy(loadWebTransport, "WebTransportSendGroup"),
  WebTransportSendStream: lazy(loadWebTransport, "WebTransportSendStream"),
  Window: globalInterfaces.windowConstructorDescriptor,
  WritableStream: lazy(loadStreams, "WritableStream"),
  WritableStreamDefaultController: lazy(loadStreams, "WritableStreamDefaultController"),
  WritableStreamDefaultWriter: lazy(loadStreams, "WritableStreamDefaultWriter"),
  clearInterval: { value: timers.clearInterval, configurable: true, writable: true },
  clearTimeout: { value: timers.clearTimeout, configurable: true, writable: true },
  console: {
    value: new consoleModule.Console((message, level) => core.print(message, level > 1)),
    configurable: true,
    writable: true,
  },
  dispatchEvent: { value: event.EventTarget.prototype.dispatchEvent, configurable: true, writable: true },
  fetch: {
    value: (...args) => loadFetch().fetch(...args),
    configurable: true,
    writable: true,
  },
  performance: { value: performance.performance, configurable: true, writable: true },
  localStorage: {
    configurable: true,
    enumerable: true,
    get: () => loadWebStorage().localStorage(),
    set() {},
  },
  Location: locationModule.locationConstructorDescriptor,
  location: { ...locationModule.locationDescriptor, configurable: true },
  navigator: { value: navigator, configurable: true },
  reportError: { value: event.reportError, configurable: true, writable: true },
  removeEventListener: { value: event.EventTarget.prototype.removeEventListener, configurable: true, writable: true },
  setInterval: { value: timers.setInterval, configurable: true, writable: true },
  setTimeout: { value: timers.setTimeout, configurable: true, writable: true },
  sessionStorage: {
    configurable: true,
    enumerable: true,
    get: () => loadWebStorage().sessionStorage(),
    set() {},
  },
  structuredClone: { value: structuredCloneModule.structuredClone, configurable: true, writable: true },
  [webidl.brand]: { value: webidl.brand, configurable: true },
});

event.setEventTargetData(globalThis);
event.saveGlobalThisReference(globalThis);
delete globalThis.Deno;
