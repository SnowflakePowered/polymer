import { onMount } from "solid-js";
import { Icon, IconButton, TextField, useInputScope } from "../../ui";
import { useShell } from "../../app/shell-context";
import { css } from "../../../styled-system/css";

const popup = css({
  position: "absolute", top: "3rem", left: "0", right: "0", zIndex: "10", display: "flex", width: "min(36rem, calc(100% - 4rem))", alignItems: "center", gap: ".75rem",
  marginInline: "auto", padding: ".4rem .4rem .4rem 1.25rem", border: "2px solid token(colors.focus)", borderRadius: "pill", bg: "raised",
  boxShadow: "0 1rem 2.5rem rgb(0 0 0 / 40%), 0 .25rem .75rem rgb(0 0 0 / 25%)",
  "& > svg": { flex: "none" },
  "& .hz-field": { display: "flex", flex: "1", minWidth: "0" },
  "& .hz-field > label": { position: "absolute", width: "1px", height: "1px", overflow: "hidden", clip: "rect(0 0 0 0)", whiteSpace: "nowrap" },
  "& .hz-field input:not(.hz-hidden-input)": { minHeight: "2.5rem", padding: "0", border: "0", bg: "transparent", fontSize: "1.2rem", outline: "none" },
  "& .hz-button": { width: "2.5rem", minHeight: "2.5rem", padding: "0", border: "0", borderRadius: "pill", bg: "transparent" },
});

export function LibrarySearch() {
  const shell = useShell();
  let root!: HTMLDivElement;
  const close = () => {
    shell.setModel("librarySearchOpen", false);
    shell.queryLibrary({ text: "" });
  };
  onMount(() => requestAnimationFrame(() => root.querySelector("input")?.focus()));
  useInputScope(() => ({ root: root.closest<HTMLElement>(".horizon-library") ?? undefined, handle: action => {
    if (action.type !== "back") return false;
    close();
    return true;
  } }));
  return <div ref={root} class={popup} role="search">
    <Icon name="search" tone="muted" />
    <TextField label="Search" placeholder="Search titles" value={shell.model.libraryQuery.text} onValueChange={text => shell.queryLibrary({ text })} />
    <IconButton icon="close" label="Close search" onClick={close} />
  </div>;
}
