import "./EmptyLibrary.css";

export function EmptyLibrary(props: { dropActive: boolean }) {
  return <section class="horizon-empty" data-active={props.dropActive}>
    <span class="horizon-empty-mark" aria-hidden="true">＋</span>
    <p>{props.dropActive ? "Drop to add games or firmware." : "Drop games or firmware to get started."}</p>
  </section>;
}
