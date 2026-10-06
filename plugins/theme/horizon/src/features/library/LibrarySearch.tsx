import { onMount } from "solid-js";
import { IconButton, TextField } from "../../ui";
import { useShell } from "../../app/shell-context";
import { css } from "../../../styled-system/css";

const popup = css({
  position: "absolute", top: "1rem", left: "50%", zIndex: "10", display: "flex", width: "min(32rem, calc(100% - 3rem))", alignItems: "center", gap: ".5rem",
  padding: ".5rem", borderRadius: "card", bg: "raised", boxShadow: "0 0.45rem 1.15rem rgb(0 0 0 / 35%)", transform: "translateX(-50%)",
  "& .hz-field": { display: "flex", flex: "1" },
  "& .hz-field > label": { position: "absolute", width: "1px", height: "1px", overflow: "hidden", clip: "rect(0 0 0 0)", whiteSpace: "nowrap" },
});

export function LibrarySearch() {
  const shell = useShell();
  let root!: HTMLDivElement;
  const close = () => {
    shell.setModel("librarySearchOpen", false);
    shell.queryLibrary({ text: "" });
  };
  onMount(() => requestAnimationFrame(() => root.querySelector("input")?.focus()));
  return <div ref={root} class={popup} role="search" onKeyDown={event => { if (event.key === "Escape") { event.stopPropagation(); close(); } }}>
    <TextField label="Search" placeholder="Search titles" value={shell.model.libraryQuery.text} onValueChange={text => shell.queryLibrary({ text })} />
    <IconButton icon="close" label="Close search" onClick={close} />
  </div>;
}
