import { Show } from "solid-js";
import "./StatusBanner.css";

export function StatusBanner(props: { message?: string; tone?: "error" | "info" }) {
  return <Show when={props.message}><p class="horizon-status" data-tone={props.tone || "info"} role={props.tone === "error" ? "alert" : "status"}>{props.message}</p></Show>;
}
