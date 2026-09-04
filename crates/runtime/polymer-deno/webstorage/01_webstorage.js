// Polymer deliberately exposes Web Storage as an asynchronous API because
// its persistent implementation is backed by Turso's asynchronous interface.
(function () {
const { core } = __bootstrap;
const { ops } = core;

class Storage {
  #persistent;

  constructor(persistent) {
    this.#persistent = persistent;
  }

  get length() {
    return ops.op_webstorage_length(this.#persistent);
  }

  key(index) {
    return ops.op_webstorage_key(this.#persistent, Number(index) >>> 0);
  }

  getItem(key) {
    return ops.op_webstorage_get_item(this.#persistent, String(key));
  }

  setItem(key, value) {
    return ops.op_webstorage_set_item(
      this.#persistent,
      String(key),
      String(value),
    );
  }

  removeItem(key) {
    return ops.op_webstorage_remove_item(this.#persistent, String(key));
  }

  clear() {
    return ops.op_webstorage_clear(this.#persistent);
  }
}

let local;
function localStorage() {
  return local ??= new Storage(true);
}

let session;
function sessionStorage() {
  return session ??= new Storage(false);
}

return { localStorage, sessionStorage, Storage };
})();
