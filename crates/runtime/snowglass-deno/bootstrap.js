// The Deno extension crates register ops and internal scripts, but their
// browser globals are normally installed by deno_runtime. Install only the
// globals Snowglass documents use, leaving WebCrypto, Node and the Deno
// namespace absent by construction.
import { core } from "ext:core/mod.js";

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

let streams;
const loadStreams = () =>
  streams ??= core.loadExtScript("ext:deno_web/06_streams.js");
let headers;
const loadHeaders = () =>
  headers ??= core.loadExtScript("ext:deno_fetch/20_headers.js");
let formData;
const loadFormData = () =>
  formData ??= core.loadExtScript("ext:deno_fetch/21_formdata.js");
let request;
const loadRequest = () =>
  request ??= core.loadExtScript("ext:deno_fetch/23_request.js");
let response;
const loadResponse = () =>
  response ??= core.loadExtScript("ext:deno_fetch/23_response.js");
let fetchModule;
const loadFetch = () =>
  fetchModule ??= core.loadExtScript("ext:deno_fetch/26_fetch.js");
let encoding;
const loadEncoding = () =>
  encoding ??= core.loadExtScript("ext:deno_web/08_text_encoding.js");
const loadWebSocket = core.createLazyLoader(
  "ext:deno_websocket/01_websocket.js",
);
const loadWebSocketStream = core.createLazyLoader(
  "ext:deno_websocket/02_websocketstream.js",
);
let webStorage;
const loadWebStorage = () =>
  webStorage ??= core.loadExtScript("ext:deno_webstorage/01_webstorage.js");

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

Object.defineProperties(globalThis, {
  AbortController: { value: abortSignal.AbortController, configurable: true, writable: true },
  AbortSignal: { value: abortSignal.AbortSignal, configurable: true, writable: true },
  Blob: { value: file.Blob, configurable: true, writable: true },
  CloseEvent: { value: event.CloseEvent, configurable: true, writable: true },
  CustomEvent: { value: event.CustomEvent, configurable: true, writable: true },
  DOMException: { value: domException.DOMException, configurable: true, writable: true },
  ErrorEvent: { value: event.ErrorEvent, configurable: true, writable: true },
  Event: { value: event.Event, configurable: true, writable: true },
  EventTarget: { value: event.EventTarget, configurable: true, writable: true },
  File: { value: file.File, configurable: true, writable: true },
  FormData: lazy(loadFormData, "FormData"),
  Headers: lazy(loadHeaders, "Headers"),
  MessageChannel: { value: messagePort.MessageChannel, configurable: true, writable: true },
  MessageEvent: { value: event.MessageEvent, configurable: true, writable: true },
  MessagePort: { value: messagePort.MessagePort, configurable: true, writable: true },
  Performance: { value: performance.Performance, configurable: true, writable: true },
  ProgressEvent: { value: event.ProgressEvent, configurable: true, writable: true },
  ReadableStream: lazy(loadStreams, "ReadableStream"),
  Request: lazy(loadRequest, "Request"),
  Response: lazy(loadResponse, "Response"),
  Storage: lazy(loadWebStorage, "Storage"),
  TextDecoder: lazy(loadEncoding, "TextDecoder"),
  TextEncoder: lazy(loadEncoding, "TextEncoder"),
  TransformStream: lazy(loadStreams, "TransformStream"),
  URL: { value: url.URL, configurable: true, writable: true },
  URLSearchParams: { value: url.URLSearchParams, configurable: true, writable: true },
  WebSocket: lazy(loadWebSocket, "WebSocket"),
  WebSocketStream: lazy(loadWebSocketStream, "WebSocketStream"),
  WritableStream: lazy(loadStreams, "WritableStream"),
  clearInterval: { value: timers.clearInterval, configurable: true, writable: true },
  clearTimeout: { value: timers.clearTimeout, configurable: true, writable: true },
  console: {
    value: new consoleModule.Console((message, level) => core.print(message, level > 1)),
    configurable: true,
    writable: true,
  },
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
  reportError: { value: event.reportError, configurable: true, writable: true },
  setInterval: { value: timers.setInterval, configurable: true, writable: true },
  setTimeout: { value: timers.setTimeout, configurable: true, writable: true },
  sessionStorage: {
    configurable: true,
    enumerable: true,
    get: () => loadWebStorage().sessionStorage(),
    set() {},
  },
  structuredClone: { value: messagePort.structuredClone, configurable: true, writable: true },
  [webidl.brand]: { value: webidl.brand, configurable: true },
});

event.setEventTargetData(globalThis);
