(() => {
  "use strict";

  globalThis.__polymerCreateEventDispatch = (webEvents, ops) => {
    const { dispatch: dispatchNativeEvent, listenerCount } = webEvents;
    const nativeTimeStamp = Symbol("nativeTimeStamp");
    const PREDICTION_HISTORY_MS = 50;
    const PREDICTION_SAMPLES = 8;
    const PREDICTION_STEPS_MS = [8, 16];

    // deno_web supplies Event, EventTarget, CustomEvent and MessageEvent. These
    // two browser DOM subclasses are specific to the Blitz document adapter.
    class MouseEvent extends Event {
      #timeStamp;

      constructor(type, init = {}) {
        super(type, { bubbles: true, cancelable: true, ...init });
        this.#timeStamp = Number.isFinite(init[nativeTimeStamp]) ? init[nativeTimeStamp] : performance.now();
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

      get timeStamp() {
        return this.#timeStamp;
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
      #coalesced;
      #predicted;

      constructor(type, init = {}) {
        super(type, init);
        this.pointerId = Number(init.pointerId) || 1;
        this.pointerType = String(init.pointerType || "mouse");
        this.isPrimary = init.isPrimary !== false;
        this.pressure = Number(init.pressure) || 0;
        this.#coalesced = Array.from(init.coalescedEvents ?? []);
        this.#predicted = Array.from(init.predictedEvents ?? []);
      }

      getCoalescedEvents() {
        return [...this.#coalesced];
      }

      getPredictedEvents() {
        return [...this.#predicted];
      }
    }

    // Extrapolates each pointer's least-squares velocity over its recent moves.
    class PointerPredictor {
      #history = new Map();

      record(pointerId, samples) {
        const now = performance.now();
        const history = [...(this.#history.get(pointerId) ?? []), ...samples]
          .filter((sample) => now - sample.time <= PREDICTION_HISTORY_MS)
          .slice(-PREDICTION_SAMPLES);
        this.#history.set(pointerId, history);
        return this.#predict(history);
      }

      #predict(history) {
        const last = history.at(-1);
        if (history.length < 2 || last.time - history[0].time < 1) return [];
        const mean = (key) => history.reduce((sum, sample) => sum + sample[key], 0) / history.length;
        const [time, x, y] = [mean("time"), mean("x"), mean("y")];
        const spread = history.reduce((sum, sample) => sum + (sample.time - time) ** 2, 0);
        const slope = (key, center) =>
          history.reduce((sum, sample) => sum + (sample.time - time) * (sample[key] - center), 0) / spread;
        const [velocityX, velocityY] = [slope("x", x), slope("y", y)];
        return PREDICTION_STEPS_MS.map((step) => ({
          time: last.time + step,
          x: last.x + velocityX * step,
          y: last.y + velocityY * step,
        }));
      }
    }

    const predictor = new PointerPredictor();

    // Native pointer moves carry `samples` as [clientX, clientY, ageMs], the
    // last being the move itself.
    function createPointerEvent(type, init) {
      const { samples, ...eventInit } = init;
      if (type !== "pointermove" || !Array.isArray(samples) || samples.length === 0) {
        return new PointerEvent(type, eventInit);
      }
      const now = performance.now();
      const timed = samples.map(([x, y, age]) => ({ time: now - age, x, y }));
      const sampleEvent = ({ time, x, y }) =>
        new PointerEvent(type, {
          ...eventInit,
          bubbles: false,
          cancelable: false,
          clientX: x,
          clientY: y,
          screenX: x,
          screenY: y,
          [nativeTimeStamp]: time,
        });
      return new PointerEvent(type, {
        ...eventInit,
        coalescedEvents: timed.map(sampleEvent),
        predictedEvents: predictor.record(Number(eventInit.pointerId) || 1, timed).map(sampleEvent),
        [nativeTimeStamp]: timed.at(-1).time,
      });
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
      createPointerEvent,
      dispatch,
      dispatchNativeEvent,
    });
  };
})();
