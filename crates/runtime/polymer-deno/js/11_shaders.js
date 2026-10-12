import { op_shaders_resolve, op_shaders_search } from "ext:core/ops";

export function createShaders() {
  return Object.freeze({
    list: () => op_shaders_search("", 0xffffffff),
    search: (query, options = {}) =>
      op_shaders_search(String(query ?? ""), Math.max(0, Math.trunc(Number(options.limit ?? 50)))),
    resolve: (relativePath) => op_shaders_resolve(String(relativePath)),
  });
}
