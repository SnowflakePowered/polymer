import "./EmptyLibrary.css";

export function EmptyLibrary(props: { dropActive: boolean }) {
  return <section class="horizon-empty" data-active={props.dropActive}>
    <span class="horizon-empty-mark" aria-hidden="true">＋</span>
    <h1>{props.dropActive ? "Release to start" : "Your classics are ready when you are"}</h1>
    <p>{props.dropActive ? "Polymer will detect and launch this game." : "Drop a game or game folder anywhere in this area."}</p>
  </section>;
}
