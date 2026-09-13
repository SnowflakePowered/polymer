import { useShell } from "../../app/shell-context";
import "./LaunchingPage.css";

export function LaunchingPage() {
  const shell = useShell();
  const preparing = () => shell.model.launcher.kind === "detecting";
  return <section class="horizon-launching" aria-live="polite"><div>
    <p>{preparing() ? "Preparing game…" : "Launching game…"}</p>
    <div role="progressbar" aria-label={preparing() ? "Preparing game" : "Launching game"}><span /></div>
  </div></section>;
}
