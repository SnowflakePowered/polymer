import { For, Show, splitProps, type JSX, type ParentProps } from "solid-js";
import { useFocusRing, useInputScope, useNavigation, type ActionHint, type InputAction } from "./input";
import { ButtonGlyph } from "./media";

export function Screen(props: ParentProps<{ class?: string; hints?: readonly ActionHint[]; onAction?: (action: InputAction) => boolean }>) {
  let root!: HTMLElement;
  useInputScope(() => ({ root, navigation: true, hints: () => props.hints ?? [], handle: action => props.onAction?.(action) ?? false }));
  return <main ref={root} class={`hz-screen ${props.class ?? ""}`}>{props.children}</main>;
}
export function ScreenHeader(props: ParentProps<{ title: string; description?: string; icon?: JSX.Element }>) {
  return <header class="hz-screen-header">{props.icon}<div><h1>{props.title}</h1><Show when={props.description}><p>{props.description}</p></Show></div>{props.children}</header>;
}
export function NavigationRail(props: JSX.HTMLAttributes<HTMLElement>) {
  const [local, rest] = splitProps(props, ["class"]);
  return <aside {...rest} class={`hz-navigation-rail ${local.class ?? ""}`} />;
}
export function MasterDetail(props: ParentProps<{ navigation: JSX.Element; detail?: JSX.Element }>) {
  return <div class="hz-master-detail"><aside class="hz-master-pane">{props.navigation}</aside><section class="hz-content-pane">{props.children}</section><Show when={props.detail}><aside class="hz-detail-pane">{props.detail}</aside></Show></div>;
}
export function Section(props: ParentProps<{ title: string; description?: string }>) {
  return <section class="hz-section"><h2>{props.title}</h2><Show when={props.description}><p class="hz-description">{props.description}</p></Show>{props.children}</section>;
}
export function SettingRow(props: ParentProps<{ description?: string }>) {
  let row!: HTMLDivElement;
  useFocusRing(() => row, () => document.activeElement?.closest(".hz-slider") ? null : row);
  return <div ref={row} class="hz-setting-row">
    {props.children}<Show when={props.description}><p class="hz-description">{props.description}</p></Show>
  </div>;
}
export function DetailList(props: { items: readonly { label: string; value: JSX.Element }[] }) {
  return <dl class="hz-details"><For each={props.items}>{item => <div><dt>{item.label}</dt><dd>{item.value}</dd></div>}</For></dl>;
}
export function Divider() { return <hr class="hz-divider" />; }
const keys = { move: "↑↓←→", activate: "Enter", back: "Esc", details: "Shift", pause: "Esc", previous: "PageUp", next: "PageDown" };
const glyphs = { move: "dpad", activate: "a", back: "b", details: "x", pause: "home", previous: "l", next: "r" } as const;
export function ActionHints(props: { hints?: readonly ActionHint[]; floating?: boolean }) {
  const navigation = useNavigation();
  return <footer class="hz-hints" data-floating={props.floating || undefined}><For each={props.hints ?? navigation.hints()}>{hint => <span><Show when={navigation.origin() === "controller"} fallback={<kbd>{hint.keyboard ?? keys[hint.action]}</kbd>}><ButtonGlyph button={glyphs[hint.action]} /></Show>{hint.label}</span>}</For></footer>;
}
