interface Window {
  __snowglassBackgroundMessage(message: unknown): void;
}

window.__snowglassBackgroundMessage = (message) => {
  dispatchEvent(new MessageEvent("message", { data: message, origin: location.origin }));
};
