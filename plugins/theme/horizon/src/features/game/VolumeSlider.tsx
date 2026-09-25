import { Slider } from "../../ui";
export function VolumeSlider(props: { value: number; onChange(value: number): void }) {
  return <Slider label="Volume" min={0} max={100} step={5} format={value => `${value}%`} value={Math.round(props.value * 100)} onValueChange={value => props.onChange(value / 100)} />;
}
