import { registerSurface } from "./00_infra.js";
import { createScraping } from "./01_scraping.js";
import { createPlugin } from "./02_plugin.js";
import { createConfiguration } from "./03_configuration.js";
import { createInput } from "./04_input.js";
import { createWebRtc } from "./05_webrtc.js";
import { createEmulator } from "./06_emulator.js";
import { createConsoleStorage } from "./08_consolestorage.js";
import { createControllers } from "./07_controllers.js";
import { createLibrary } from "./09_library.js";
import { op_controller_api_snapshot, op_snowglass_next_notification } from "ext:core/ops";

const scraping = createScraping();
const plugin = createPlugin();
const configuration = createConfiguration(scraping, plugin);
const initialControllerApi = op_controller_api_snapshot();
const input = createInput();
const controllers = createControllers(initialControllerApi.controllers);
const webrtc = createWebRtc();
const emulator = createEmulator(webrtc);
const consolestorage = createConsoleStorage();
const library = createLibrary();

const Snowglass = Object.freeze({
  input: input.snowglassInput,
  controllers: controllers.controllers,
  plugin,
  scraping: scraping.Scraping,
  configuration,
  emulator: emulator.Emulator,
  webrtc: webrtc.WebRTC,
  consolestorage: consolestorage.ConsoleStorage,
  library: library.Library,
});

function dispatch(event) {
  switch (event?.type) {
    case "openRequested":
      scraping.request(event.detail);
      break;
    case "dropActiveChanged":
      scraping.setDropActive(event.detail);
      break;
    case "inputConfigurationChanged":
      break;
    case "inputConfigurationSettled":
      break;
    case "emulatorInstance":
      emulator.dispatch(event.detail);
      break;
    case "controllerApiStateChanged":
      controllers.setState(event.detail);
      break;
    case "inputChanged":
      input.snowglassInput.inputChanged(event.detail);
      break;
    case "gameSaved":
      consolestorage.gameSaved(event.detail);
      break;
    case "scrapeJobChanged":
      scraping.jobChanged(event.detail);
      break;
    case "libraryContentChanged":
      library.contentChanged(event.detail);
      break;
    default:
      throw new TypeError(`Unknown Snowglass event '${event?.type}'`);
  }
}

async function receiveNotifications() {
  while (true) {
    const notification = await op_snowglass_next_notification();
    if (notification === null) return;
    dispatch(notification);
  }
}

Object.defineProperty(globalThis, "Snowglass", {
  value: Snowglass,
  enumerable: true,
  configurable: false,
  writable: false,
});

Object.defineProperty(globalThis, "__snowglassApi", {
  value: Object.freeze({ registerSurface }),
  enumerable: false,
  configurable: false,
  writable: false,
});

void receiveNotifications();
