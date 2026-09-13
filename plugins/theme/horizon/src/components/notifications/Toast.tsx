import type { ParentProps } from "solid-js";
import { Icon } from "../Icon";
import "./Toast.css";
export function Toast(props: ParentProps<{ title: string; message: string; error?: boolean; onDismiss(): void }>) {
  return <section class="horizon-toast" classList={{ error: props.error }} role={props.error ? "alert" : "status"}>
    <Icon name="info" />
    <div class="horizon-toast-content"><strong>{props.title}</strong><p>{props.message}</p>{props.children}</div>
    <button class="horizon-toast-dismiss" type="button" aria-label={`Dismiss ${props.title}`} onClick={props.onDismiss}><Icon name="close" /></button>
  </section>;
}
