import { EmptyState } from "../../ui";
import "./EmptyLibrary.css";
export function EmptyLibrary(props: { dropActive: boolean }) {
  return <section class="horizon-empty" data-active={props.dropActive}><EmptyState title={props.dropActive ? "Drop to add games or firmware." : "Drop games or firmware to get started."} /></section>;
}
