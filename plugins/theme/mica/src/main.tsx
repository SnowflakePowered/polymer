import { EnvironmentProvider } from "@ark-ui/solid";
import { render } from "solid-js/web";
import "@polymer-fe/frontend";
import "./ui/mica.css";
import { MicaApp } from "./app/MicaApp";

const root = document.getElementById("root");
if (!root) throw new Error("Mica root element is missing");

if (window.Polymer?.meta.debug) document.body.setAttribute("data-debug", "true");

let dispose = render(() => <EnvironmentProvider value={() => document}><MicaApp /></EnvironmentProvider>, root);

if (import.meta.hot) {
  import.meta.hot.accept("./app/MicaApp", module => {
    if (!module) return;
    dispose();
    const App = module.MicaApp;
    dispose = render(() => <EnvironmentProvider value={() => document}><App /></EnvironmentProvider>, root);
  });
  import.meta.hot.dispose(() => dispose());
}
