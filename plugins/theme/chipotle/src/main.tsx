import { render } from "solid-js/web";
import { EnvironmentProvider } from "@ark-ui/solid";
import { ShellRouter } from "./solid-app";
import "../../../../js/polymer";
import "./shell.css";

const root = document.getElementById("root");
if (!root) throw new Error("Polymer root element is missing");

render(() => (
  <EnvironmentProvider value={() => document}>
    <ShellRouter />
  </EnvironmentProvider>
), root);
