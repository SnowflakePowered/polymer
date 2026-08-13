(() => {
  "use strict";

  globalThis.__snowglassCreateEventDispatch = (webEvents, ops) => {
    const { dispatch: dispatchNativeEvent, listenerCount } = webEvents;

    // deno_web supplies Event, EventTarget, CustomEvent and MessageEvent. These
    // two browser DOM subclasses are specific to the Blitz document adapter.
    class MouseEvent extends Event {
      constructor(type, init = {}) {
        super(type, { bubbles: true, cancelable: true, ...init });
        this.clientX = Number(init.clientX) || 0;
        this.clientY = Number(init.clientY) || 0;
        this.screenX = Number(init.screenX) || this.clientX;
        this.screenY = Number(init.screenY) || this.clientY;
        this.button = Number(init.button) || 0;
        this.buttons = Number(init.buttons) || 0;
        this.ctrlKey = Boolean(init.ctrlKey);
        this.shiftKey = Boolean(init.shiftKey);
        this.altKey = Boolean(init.altKey);
        this.metaKey = Boolean(init.metaKey);
      }

      get x() {
        return this.clientX;
      }

      get y() {
        return this.clientY;
      }
    }

    class KeyboardEvent extends Event {
      constructor(type, init = {}) {
        super(type, { bubbles: true, cancelable: true, ...init });
        this.key = String(init.key || "");
        this.code = String(init.code || "");
        this.repeat = Boolean(init.repeat);
        this.altKey = Boolean(init.altKey);
        this.ctrlKey = Boolean(init.ctrlKey);
        this.shiftKey = Boolean(init.shiftKey);
        this.metaKey = Boolean(init.metaKey);
      }
    }

    function dispatch(target, event) {
      let handled = false;
      for (let current = target; current; current = current.parentNode) {
        if (listenerCount(current, event.type) > 0) {
          handled = true;
          break;
        }
      }
      dispatchNativeEvent(target, event);
      ops.op_event_result(handled, event.defaultPrevented);
    }

    return Object.freeze({
      MouseEvent,
      KeyboardEvent,
      dispatch,
      dispatchNativeEvent,
    });
  };
})();
