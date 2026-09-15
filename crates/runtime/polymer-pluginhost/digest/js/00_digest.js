import { op_digest } from "ext:core/ops";

export function createFile({ __digestToken: token, ...metadata }) {
  return Object.freeze({
    ...metadata,
    kind: "file",
    digest: async algorithm => op_digest({ type: "handle", token, binary: false }, algorithm),
  });
}

export function createPackage(value) {
  if (value == null) return value;
  const { __digestToken: token, files, ...metadata } = value;
  return Object.freeze({
    ...metadata,
    ...(files === undefined ? {} : { files: Object.freeze(files.map(createFile)) }),
    digest: async algorithm => op_digest({ type: "handle", token, binary: false }, algorithm),
    binary: Object.freeze({
      digest: async algorithm => op_digest({ type: "handle", token, binary: true }, algorithm),
    }),
  });
}
