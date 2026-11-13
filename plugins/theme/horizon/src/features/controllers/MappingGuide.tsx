import { Show } from "solid-js";
import type { InputLayout, ManagedControllerDevice } from "@polymer-fe/frontend";
import { ErrorState } from "../../ui";
import { BindCapture } from "./BindCapture";
import { ButtonGuide } from "./ButtonGuide";
import type { MappingEditor } from "./MappingEditor";
import { padFamily } from "./padGlyphs";
import { targetGlyph } from "./targetGlyphs";

export function MappingGuide(props: { editor: MappingEditor; layout: InputLayout; device?: ManagedControllerDevice }) {
  const rebind = () => props.editor.rebind();
  const target = () => rebind() ? rebind()!.keys[rebind()!.step] : "";
  return <>
    <Show when={props.editor.error()}>{message => <ErrorState message={message()} />}</Show>
    <Show when={props.editor.mapping()}>{mapping => <ButtonGuide onSelect={props.editor.select} family={padFamily(props.device)} layout={props.layout} bindings={mapping().bindings} overrides={props.editor.marksChanges() ? mapping().overrides : {}} />}</Show>
    <BindCapture open={rebind() !== undefined} current={props.editor.mapping()?.bindings[target()] ?? "none"} family={padFamily(props.device)} target={target()} label={props.editor.label(target())} glyph={targetGlyph(props.layout.id, target())} device={props.device} onCapture={bound => void props.editor.capture(bound)} onClose={props.editor.close} />
  </>;
}
