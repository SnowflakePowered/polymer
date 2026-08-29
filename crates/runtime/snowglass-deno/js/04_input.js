class InputClient extends EventTarget {
  publishState(_sources, _priority, controls) {
    for (const control of controls || []) {
      this.dispatchEvent(new CustomEvent("controllerinput", {
        detail: Object.freeze({ ...control }),
      }));
    }
  }
}

export function createInput() {
  const client = new InputClient();
  return Object.freeze({
    snowglassInput: client,
    setState: (sources, priority, controls) => client.publishState(sources, priority, controls),
  });
}
