import { registerSurface } from "./00_infra.js";
import { createScraping } from "./01_scraping.js";
import { createPlugin } from "./02_plugin.js";
import { createConfiguration } from "./03_configuration.js";
import { createInput } from "./04_input.js";
import { createEmulator } from "./06_emulator.js";
import { createConsoleStorage } from "./08_consolestorage.js";
import { createControllers } from "./07_controllers.js";
import { createLibrary } from "./09_library.js";
import { createJobs } from "./10_jobs.js";
import { createShaders } from "./11_shaders.js";
import { op_controller_api_snapshot, op_machine, op_polymer_next_notification } from "ext:core/ops";

const scraping = createScraping();
const plugin = createPlugin();
const configuration = createConfiguration(plugin.Plugin);
const initialControllerApi = op_controller_api_snapshot();
const input = createInput();
const controllers = createControllers(initialControllerApi.controllers);
const emulator = createEmulator();
const library = createLibrary();
const consolestorage = createConsoleStorage(library);
const jobs = createJobs();
const shaders = createShaders();

const Polymer = Object.freeze({
  meta: { debug: false },
  input: input.polymerInput,
  controllers: controllers.controllers,
  plugin: plugin.Plugin,
  scraping: scraping.Scraping,
  configuration,
  emulator: emulator.Emulator,
  consolestorage: consolestorage.ConsoleStorage,
  library: library.Library,
  jobs: jobs.Jobs,
  shaders,
  machine: (id) => op_machine(String(id)),
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
      input.polymerInput.inputChanged(event.detail);
      break;
    case "gameSaved":
      consolestorage.gameSaved(event.detail);
      break;
    case "pluginEvent":
      plugin.dispatch(event.detail);
      break;
    case "jobChanged":
      jobs.changed(event.detail);
      break;
    case "libraryContentChanged":
      library.contentChanged(event.detail);
      break;
    default:
      throw new TypeError(`Unknown Polymer event '${event?.type}'`);
  }
}

async function receiveNotifications() {
  while (true) {
    const notification = await op_polymer_next_notification();
    if (notification === null) return;
    dispatch(notification);
  }
}

Object.defineProperty(globalThis, "Polymer", {
  value: Polymer,
  enumerable: true,
  configurable: false,
  writable: false,
});

Object.defineProperty(globalThis, "__polymerApi", {
  value: Object.freeze({ registerSurface }),
  enumerable: false,
  configurable: false,
  writable: false,
});

void receiveNotifications();
