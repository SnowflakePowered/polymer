import { EnvironmentProvider } from "@ark-ui/solid";
import "./index.css";
import { render } from "solid-js/web";
import "@polymer-fe/frontend";
import { ShellRouter } from "./app/ShellRouter";

const root = document.getElementById("root");
if (!root) throw new Error("Horizon root element is missing");

if (window.Polymer?.meta.debug) document.body.setAttribute("data-debug", "true");

let dispose = render(() => <EnvironmentProvider value={() => document}><ShellRouter /></EnvironmentProvider>, root);

// The embedded runtime cannot use location.reload(). Remount the shell when
// an update crosses Solid Refresh's component boundaries instead.
if (import.meta.hot) {
  import.meta.hot.accept("./app/ShellRouter", module => {
    if (!module) return;
    dispose();
    const Router = module.ShellRouter;
    dispose = render(() => <EnvironmentProvider value={() => document}><Router /></EnvironmentProvider>, root);
  });
  import.meta.hot.dispose(() => dispose());
}
