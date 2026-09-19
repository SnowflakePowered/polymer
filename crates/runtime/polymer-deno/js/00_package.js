import { op_digest } from "ext:core/ops";

// Package metadata and its private host representation belong to the package.
export class Package {
  #data;

  constructor(data) {
    this.#data = Object.freeze({ ...data });
    Object.assign(this, this.toJSON());
    Object.freeze(this);
  }

  async digest(algorithm) {
    return op_digest({ type: "path", path: this.#data.__digestPath, binary: false }, algorithm);
  }

  get binary() {
    return Object.freeze({
      digest: async algorithm => op_digest({ type: "path", path: this.#data.__digestPath, binary: true }, algorithm),
    });
  }

  toJSON() {
    const { __digestPath, __manifest, binaryDigest, ...metadata } = this.#data;
    return metadata;
  }

  static serialize(value) {
    if (value instanceof Package) return value.#data;
    if (Array.isArray(value)) return value.map(child => this.serialize(child));
    if (value === null || typeof value !== "object") return value;
    return Object.fromEntries(Object.entries(value).map(([key, child]) => [key, this.serialize(child)]));
  }
}
