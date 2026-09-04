import { op_input_get } from "ext:core/ops";

class InputClient extends EventTarget {
  get(index) {
    return op_input_get(index);
  }

  inputChanged(detail) {
    this.dispatchEvent(new CustomEvent("input", { detail: Object.freeze(detail) }));
  }
}

export function createInput() {
  const client = new InputClient();
  return Object.freeze({ polymerInput: client });
}
