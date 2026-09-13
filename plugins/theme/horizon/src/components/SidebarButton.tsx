import type { ParentProps } from "solid-js";
import { Icon, type IconName } from "./Icon";
import "./SidebarButton.css";

export function SidebarButton(props: ParentProps<{ icon: IconName; label: string; active?: boolean; onClick?: () => void }>) {
  return <button type="button" class="horizon-sidebar-button" classList={{ active: props.active }} aria-label={props.label} title={props.label} onClick={props.onClick}><Icon name={props.icon} />{props.children}</button>;
}
