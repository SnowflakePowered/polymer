import {
  op_library_games_list,
  op_library_games_get,
  op_library_games_delete,
  op_library_launchables_add,
  op_library_launchables_delete,
  op_library_metadata_add,
  op_library_metadata_update,
  op_library_metadata_delete,
  op_library_metadata_replace,
  op_library_metadata_text,
  op_library_metadata_url,
} from "ext:core/ops";

const relations = new Set([
  "title", "description", "releasedate", "publisher", "boxartfront",
  "boxartback", "boxartspine", "boxartfull", "screenshot", "logo",
  "video", "icon", "banner", "media",
]);

function uuid(value, name) {
  if (typeof value !== "string" || !/^[0-9a-f]{32}$/.test(value)) {
    throw new TypeError(`${name} must be a lowercase 32-character UUID`);
  }
  return value;
}

function optionalString(value, name) {
  if (value === undefined || value === null) return null;
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`${name} must be a non-empty string`);
  }
  return value;
}

function metadataInput(input, sourceOverride) {
  if (input === null || typeof input !== "object") {
    throw new TypeError("metadata input must be an object");
  }
  if (!relations.has(input.relation)) {
    throw new TypeError(`unknown metadata relation '${input.relation}'`);
  }
  const source = sourceOverride ?? optionalString(input.source, "source");
  if (source === null) throw new TypeError("source is required");
  const language = optionalString(input.language, "language");
  const data = input.data;
  if (data === null || typeof data !== "object") {
    throw new TypeError("metadata data must be an object");
  }
  const result = { relation: input.relation, language, source, kind: data.kind };
  switch (data.kind) {
    case "text":
      if (typeof data.value !== "string") throw new TypeError("text metadata value must be a string");
      result.value = data.value;
      break;
    case "year":
      if (!Number.isInteger(data.value) || data.value < 1 || data.value > 9999) {
        throw new TypeError("release year must be an integer from 1 through 9999");
      }
      result.value = data.value;
      break;
    case "inline":
      if (!(data.bytes instanceof Uint8Array)) throw new TypeError("inline metadata bytes must be a Uint8Array");
      result.mediaType = optionalString(data.mediaType, "mediaType");
      if (result.mediaType === null) throw new TypeError("mediaType is required");
      result.bytes = Array.from(data.bytes);
      break;
    case "object":
      result.mediaType = optionalString(data.mediaType, "mediaType");
      result.path = optionalString(data.path, "path");
      if (result.mediaType === null) throw new TypeError("mediaType is required");
      if (result.path === null) throw new TypeError("path is required");
      break;
    default:
      throw new TypeError(`unknown metadata data kind '${data.kind}'`);
  }
  return result;
}

export function createLibrary() {
  const games = Object.freeze({
    list(options = {}) {
      return op_library_games_list(optionalString(options.platform, "platform"));
    },
    get(rootBinaryId) {
      return op_library_games_get(uuid(rootBinaryId, "rootBinaryId"));
    },
    delete(rootBinaryId) {
      return op_library_games_delete(uuid(rootBinaryId, "rootBinaryId"));
    },
  });
  const launchables = Object.freeze({
    add(input) {
      if (input === null || typeof input !== "object") throw new TypeError("launchable input must be an object");
      const path = optionalString(input.path, "path");
      if (path === null) throw new TypeError("path is required");
      return op_library_launchables_add(
        path,
        optionalString(input.platform, "platform"),
        input.parentBinaryId == null ? null : uuid(input.parentBinaryId, "parentBinaryId"),
      );
    },
    delete(id) {
      return op_library_launchables_delete(uuid(id, "id"));
    },
  });
  const metadata = Object.freeze({
    add(rootBinaryId, input) {
      return op_library_metadata_add(uuid(rootBinaryId, "rootBinaryId"), metadataInput(input));
    },
    update(id, input) {
      return op_library_metadata_update(uuid(id, "id"), metadataInput(input));
    },
    delete(id) {
      return op_library_metadata_delete(uuid(id, "id"));
    },
    replace(rootBinaryId, source, inputs) {
      if (!Array.isArray(inputs)) throw new TypeError("inputs must be an array");
      source = optionalString(source, "source");
      if (source === null) throw new TypeError("source is required");
      return op_library_metadata_replace(
        uuid(rootBinaryId, "rootBinaryId"),
        source,
        inputs.map((input) => metadataInput(input, source)),
      );
    },
    text(id) {
      return op_library_metadata_text(uuid(id, "id"));
    },
    url(id) {
      return op_library_metadata_url(uuid(id, "id"));
    },
  });
  return Object.freeze({ games, launchables, metadata });
}
