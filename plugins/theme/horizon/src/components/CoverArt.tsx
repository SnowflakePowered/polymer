import { Show, createSignal } from "solid-js";
import "./CoverArt.css";
/** Artwork owns loading/failure state; its parent owns layout and interaction. */
export function CoverArt(props: { src?: string; onRatio(ratio: number): void; onReady(ready: boolean): void }) {
  const [failed, setFailed] = createSignal<string>();
  const source = () => props.src !== failed() ? props.src : undefined;
  return <Show when={source()}>{url => <img class="horizon-cover-art" src={url()} alt="" loading="lazy" decoding="async"
    onLoad={event => {
      const image = event.currentTarget;
      if (!image.naturalWidth || !image.naturalHeight) return;
      props.onRatio(image.naturalWidth / image.naturalHeight);
      props.onReady(true);
    }} onError={() => { setFailed(url()); props.onReady(false); }} />}</Show>;
}
