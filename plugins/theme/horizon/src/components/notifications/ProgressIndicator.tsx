import "./ProgressIndicator.css";
export function ProgressIndicator(props: { label: string; completed?: number; total?: number }) {
  const percent = () => props.total && props.total > 0 && props.completed !== undefined ? Math.max(0, Math.min(100, props.completed / props.total * 100)) : undefined;
  return <div class="horizon-progress" role="progressbar" aria-label={props.label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent()}>
    <span classList={{ indeterminate: percent() === undefined }} style={{ width: `${percent() ?? 40}%` }} />
  </div>;
}
