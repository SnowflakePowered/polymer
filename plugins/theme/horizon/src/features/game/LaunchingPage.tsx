import { Show, createMemo, createResource } from "solid-js";
import type { LibraryShowcase } from "@polymer-fe/frontend/library-state";
import { Spinner } from "../../ui";
import { useShell } from "../../app/shell-context";
import "./LaunchingPage.css";

export function LaunchingScreen() {
  const shell = useShell();
  const preparing = () => shell.model.launcher.kind === "detecting";
  const loading = () => preparing() || shell.model.launcher.kind === "launching";
  const path = createMemo(() => shell.model.launcher.kind === "detecting" ? shell.model.launcher.path : shell.model.draft?.path);
  const name = () => shell.model.draft?.name ?? ("name" in shell.model.launcher ? shell.model.launcher.name : "");
  const [art] = createResource(path, path => shell.showcase(path).catch(() => ({} as LibraryShowcase)));
  return <Show when={loading()}>
    <section class="horizon-launching" aria-live="polite">
      <Show when={art()?.background}>{image => <img class="horizon-launching-backdrop" src={image()} alt="" />}</Show>
      <Show when={art()?.logo} fallback={<h1 class="horizon-launching-title">{name()}</h1>}>{logo => <img class="horizon-launching-logo" src={logo()} alt={name()} />}</Show>
      <span class="horizon-launching-spinner"><Spinner label={preparing() ? "Preparing game" : "Launching game"} /></span>
    </section>
  </Show>;
}
