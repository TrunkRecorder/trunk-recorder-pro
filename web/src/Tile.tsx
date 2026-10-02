// A figure in a row of tiles: a label, a value and a line under it.

export function Tile(props: { label: string; value: React.ReactNode; sub?: React.ReactNode; tone?: "ok" | "warn" | "bad" }) {
  return (
    <div className={`tile${props.tone ? ` tone-${props.tone}` : ""}`}>
      <div className="tile-label">{props.label}</div>
      <div className="tile-value">{props.value}</div>
      {props.sub && <div className="tile-sub">{props.sub}</div>}
    </div>
  );
}
