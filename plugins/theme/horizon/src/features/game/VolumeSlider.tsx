import { Slider } from "../../ui";
export function VolumeSlider(props: { value: number; onChange(value: number): void }) {
  return <Slider label="Volume" min={0} max={1} step={.01} value={props.value} onValueChange={props.onChange} />;
}
