import { Package } from "./00_package.js";
import {
  op_library_games_list, op_library_games_get, op_library_games_delete,
  op_library_launchables_add, op_library_launchables_delete,
  op_library_metadata_add, op_library_metadata_update, op_library_metadata_delete,
  op_library_metadata_replace, op_library_metadata_text, op_library_metadata_url,
} from "ext:core/ops";

function uuid(value, name) {
  if (typeof value !== "string" || !/^[0-9a-f]{32}$/.test(value)) throw new TypeError(`${name} must be a lowercase 32-character UUID`);
  return value;
}

function optionalString(value, name) {
  if (value === undefined || value === null) return null;
  if (typeof value !== "string" || value.length === 0) throw new TypeError(`${name} must be a non-empty string`);
  return value;
}

function scalar(value) {
  if (typeof value === "string" || typeof value === "boolean") return value;
  if (typeof value === "number" && Number.isFinite(value)) return value;
  throw new TypeError("metadata scalar values must be strings, finite numbers, or booleans");
}

function metadataInput(input, sourceOverride) {
  if (input === null || typeof input !== "object" || Array.isArray(input)) throw new TypeError("metadata input must be an object");
  const key = optionalString(input.key, "key");
  const source = sourceOverride ?? optionalString(input.source, "source");
  if (key === null || !key.split(".").every(part => /^[A-Za-z0-9_-]+$/.test(part))) throw new TypeError("key must be a namespaced dotted identifier");
  if (source === null) throw new TypeError("source is required");
  const data = input.data;
  if (data === null || typeof data !== "object" || Array.isArray(data)) throw new TypeError("metadata data must be an object");
  const result = { key, source, kind: data.kind };
  switch (data.kind) {
    case "scalar": result.value = scalar(data.value); break;
    case "inline":
      if (!(data.bytes instanceof Uint8Array)) throw new TypeError("inline metadata bytes must be a Uint8Array");
      result.mediaType = optionalString(data.mediaType, "mediaType");
      if (result.mediaType === null) throw new TypeError("mediaType is required");
      result.bytes = Array.from(data.bytes);
      break;
    case "object":
      result.mediaType = optionalString(data.mediaType, "mediaType");
      result.path = optionalString(data.path, "path");
      if (result.mediaType === null || result.path === null) throw new TypeError("object metadata requires mediaType and path");
      break;
    default: throw new TypeError(`unknown metadata data kind '${data.kind}'`);
  }
  return result;
}

export function createLibrary() {
  const Library = new EventTarget();
  const games = Object.freeze({
    async list(options = {}) {
      const games = await op_library_games_list(optionalString(options.platform, "platform"));
      return games.map(game => ({ ...game, launchables: game.launchables.map(value => new Package(value)) }));
    },
    async get(id) {
      const game = await op_library_games_get(uuid(id, "rootBinaryId"));
      return game == null ? game : { ...game, launchables: game.launchables.map(value => new Package(value)) };
    },
    delete(id) { return op_library_games_delete(uuid(id, "rootBinaryId")); },
  });
  const launchables = Object.freeze({
    async add(input) {
      if (input === null || typeof input !== "object") throw new TypeError("launchable input must be an object");
      const path = optionalString(input.path, "path");
      if (path === null) throw new TypeError("path is required");
      return new Package(await op_library_launchables_add(path, optionalString(input.platform, "platform"), input.parentBinaryId == null ? null : uuid(input.parentBinaryId, "parentBinaryId")));
    },
    delete(id) { return op_library_launchables_delete(uuid(id, "id")); },
  });
  const metadata = Object.freeze({
    add(root, input) { return op_library_metadata_add(uuid(root, "rootBinaryId"), metadataInput(input)); },
    update(id, input) { return op_library_metadata_update(uuid(id, "id"), metadataInput(input)); },
    delete(id) { return op_library_metadata_delete(uuid(id, "id")); },
    replace(root, source, inputs) {
      if (!Array.isArray(inputs)) throw new TypeError("inputs must be an array");
      source = optionalString(source, "source");
      if (source === null) throw new TypeError("source is required");
      return op_library_metadata_replace(uuid(root, "rootBinaryId"), source, inputs.map(input => metadataInput(input, source)));
    },
    text(id) { return op_library_metadata_text(uuid(id, "id")); },
    url(id) { return op_library_metadata_url(uuid(id, "id")); },
  });
  Object.defineProperties(Library, {
    games: { value: games, enumerable: true },
    launchables: { value: launchables, enumerable: true },
    metadata: { value: metadata, enumerable: true },
  });
  return Object.freeze({
    Library,
    contentChanged(detail) {
      Library.dispatchEvent(new CustomEvent("contentchange", { detail: Object.freeze(detail) }));
    },
  });
}
