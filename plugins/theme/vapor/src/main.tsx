import { EnvironmentProvider } from "@ark-ui/solid";
import { render } from "solid-js/web";
import "../../../../js/polymer";
import "./ui/vapor.css";
import { VaporApp } from "./app/VaporApp";

const root = document.getElementById("root");
if (!root) throw new Error("Vapor root element is missing");

if (window.Polymer?.meta.debug) document.body.setAttribute("data-debug", "true");

let dispose = render(() => <EnvironmentProvider value={() => document}><VaporApp /></EnvironmentProvider>, root);

if (import.meta.hot) {
  import.meta.hot.accept("./app/VaporApp", module => {
    if (!module) return;
    dispose();
    const App = module.VaporApp;
    dispose = render(() => <EnvironmentProvider value={() => document}><App /></EnvironmentProvider>, root);
  });
  import.meta.hot.dispose(() => dispose());
}
