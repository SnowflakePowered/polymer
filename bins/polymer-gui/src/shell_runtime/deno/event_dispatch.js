(() => {
  "use strict";

  globalThis.__polymerCreateEventDispatch = (webEvents, ops) => {
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

    class PointerEvent extends MouseEvent {
      constructor(type, init = {}) {
        super(type, init);
        this.pointerId = Number(init.pointerId) || 1;
        this.pointerType = String(init.pointerType || "mouse");
        this.isPrimary = init.isPrimary !== false;
        this.pressure = Number(init.pressure) || 0;
      }
    }

    class WheelEvent extends MouseEvent {
      constructor(type, init = {}) {
        super(type, init);
        this.deltaX = Number(init.deltaX) || 0;
        this.deltaY = Number(init.deltaY) || 0;
        this.deltaMode = Number(init.deltaMode) || 0;
      }
    }

    class FocusEvent extends Event {
      constructor(type, init = {}) {
        super(type, init);
        this.relatedTarget = init.relatedTarget || null;
      }
    }

    class InputEvent extends Event {
      constructor(type, init = {}) {
        super(type, init);
        this.data = init.data == null ? null : String(init.data);
        this.inputType = String(init.inputType || "insertText");
        this.isComposing = Boolean(init.isComposing);
      }
    }

    class CompositionEvent extends Event {
      constructor(type, init = {}) {
        super(type, init);
        this.data = String(init.data || "");
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
      // Go through the Node override. Calling deno_web's low-level dispatcher
      // directly here bypasses the DOM target's dispatch path after native
      // retargeting (notably pointer capture), so listeners on the captured
      // node never run even though listenerCount sees them.
      target.dispatchEvent(event);
      ops.op_event_result(handled, event.defaultPrevented, Boolean(event.cancelBubble));
    }

    return Object.freeze({
      MouseEvent,
      PointerEvent,
      WheelEvent,
      KeyboardEvent,
      FocusEvent,
      InputEvent,
      CompositionEvent,
      dispatch,
      dispatchNativeEvent,
    });
  };
})();
