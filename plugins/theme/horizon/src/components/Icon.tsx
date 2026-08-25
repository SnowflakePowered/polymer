import type { JSX } from "solid-js";

export type IconName = "user" | "friends" | "network" | "settings" | "back" | "play" | "info" | "close" | "gamepad" | "volume";

const paths: Record<IconName, JSX.Element> = {
  user: <><path d="M12 4a4 4 0 1 1 0 8 4 4 0 0 1 0-8Z" /><path d="M4.5 21a7.5 7.5 0 0 1 15 0" /></>,
  friends: <><path d="M9 5a3 3 0 1 1 0 6 3 3 0 0 1 0-6ZM17 6.5a2.5 2.5 0 1 1 0 5 2.5 2.5 0 0 1 0-5Z" /><path d="M2.5 20a6.5 6.5 0 0 1 13 0M14 15a5 5 0 0 1 7.5 4.3" /></>,
  network: <><path d="M12 3a9 9 0 1 1 0 18 9 9 0 0 1 0-18Z" /><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18" /></>,
  settings: <><path d="M12 9a3 3 0 1 1 0 6 3 3 0 0 1 0-6Z" /><path d="M19 13.5v-3l-2-.7-.7-1.7.9-1.9-2.1-2.1-1.9.9-1.7-.7-.7-2h-3l-.7 2-1.7.7-1.9-.9-2.1 2.1.9 1.9-.7 1.7-2 .7v3l2 .7.7 1.7-.9 1.9 2.1 2.1 1.9-.9 1.7.7.7 2h3l.7-2 1.7-.7 1.9.9 2.1-2.1-.9-1.9.7-1.7 2-.7Z" /></>,
  back: <path d="m15 5-7 7 7 7" />,
  play: <path d="m9 5 10 7-10 7Z" />,
  info: <><path d="M12 3a9 9 0 1 1 0 18 9 9 0 0 1 0-18Z" /><path d="M12 11v6M12 7h.01" /></>,
  close: <path d="m6 6 12 12M18 6 6 18" />,
  gamepad: <><path d="M7 8h10a5 5 0 0 1 4.7 3.3l1 2.8a3.5 3.5 0 0 1-5.7 3.8L15 16H9l-2 1.9a3.5 3.5 0 0 1-5.7-3.8l1-2.8A5 5 0 0 1 7 8Z" /><path d="M7 11v4M5 13h4M17 12h.01M19 14h.01" /></>,
  volume: <><path d="M4 9v6h4l5 4V5L8 9Z" /><path d="M16 9a4 4 0 0 1 0 6M18.5 6.5a8 8 0 0 1 0 11" /></>,
};

export function Icon(props: { name: IconName; class?: string }) {
  return <svg class={props.class} viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">{paths[props.name]}</svg>;
}
