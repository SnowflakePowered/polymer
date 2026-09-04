import { EnvironmentProvider } from "@ark-ui/solid";
import { render } from "solid-js/web";
import "../../../../js/polymer";
import { ShellRouter } from "./app/ShellRouter";

const root = document.getElementById("root");
if (!root) throw new Error("Horizon root element is missing");

render(() => <EnvironmentProvider value={() => document}><ShellRouter /></EnvironmentProvider>, root);
