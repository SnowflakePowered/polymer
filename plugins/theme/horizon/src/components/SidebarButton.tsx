import type { ParentProps } from "solid-js";
import { css } from "../../styled-system/css";
import { Icon, type IconName } from "../ui";

const inverted = { color: "brand", bg: "onBrand", "& [data-tone=rest]": { display: "none" }, "& [data-tone=inverted]": { display: "flex" } };
const button = css({
  display: "flex", width: "3.4rem", height: "3.4rem", flex: "none", alignItems: "center", justifyContent: "center", gap: "1rem",
  padding: "0", border: "0", borderRadius: "card", color: "onBrand", bg: "transparent", cursor: "pointer", fontSize: "1.5rem", whiteSpace: "nowrap",
  "& > span": { display: "flex" },
  "& svg": { width: "2.5rem", height: "2.5rem", flex: "none" },
  "& [data-tone=inverted]": { display: "none" },
  "& [data-label]": { display: "none" },
  "& [data-label] [data-tone=rest]": { color: "onBrand" },
  "& [data-label] [data-tone=inverted]": { color: "brand" },
  "[data-expanded] &": { width: "100%", justifyContent: "flex-start", paddingInline: "2.25rem", borderRadius: "0" },
  "[data-expanded] & [data-label]": { display: "flex" },
  "[data-expanded] &::after": { display: "none" },
  "[data-expanded] &[data-active]:not(:focus):not(:hover)": { color: "onBrand", bg: "transparent", "& [data-tone=rest]": { display: "flex" }, "& [data-tone=inverted]": { display: "none" } },
  _hover: inverted,
  _active: inverted,
  "--hz-ring-fill": "token(colors.onBrand)",
  _focus: inverted,
  _disabled: { opacity: 0.5, cursor: "default" },
});

export function SidebarButton(props: ParentProps<{ icon: IconName; iconSize?: number; label: string; active?: boolean; expanded?: boolean; onClick?: () => void }>) {
  const stroke = () => props.iconSize ? 60 / props.iconSize : undefined;
  return <button type="button" class={button} data-select-ring disabled={!props.onClick} data-active={props.active || undefined} aria-label={props.label} aria-current={props.active && props.expanded === undefined ? "page" : undefined} aria-expanded={props.expanded} title={props.label} onClick={props.onClick}>
    <span data-tone="rest"><Icon name={props.icon} tone="onBrand" strokeWidth={stroke()} /></span>
    <span data-tone="inverted"><Icon name={props.icon} tone="brand" strokeWidth={stroke()} /></span>
    <span data-label><span data-tone="rest">{props.label}</span><span data-tone="inverted">{props.label}</span></span>
    {props.children}
  </button>;
}
