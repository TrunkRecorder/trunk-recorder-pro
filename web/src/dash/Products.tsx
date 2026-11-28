// A call's products, drawn the same wherever a call is listed.
import { productText } from "../products.ts";
import type { CallEntry, Product } from "../protocol.ts";

/** The few words before a product's text: what kind, and whose. */
function facts(p: Product): { kind: string; facts: string[] } {
  switch (p.kind) {
    case "page":
      return { kind: p.data.protocol.toUpperCase(), facts: [String(p.data.address), ...(p.data.function !== undefined ? [`F${p.data.function}`] : []), ...(p.data.format === "tone" ? ["tone"] : [])] };
    case "acars":
      return { kind: "ACARS", facts: [p.data.registration, p.data.flight ?? "", p.data.label].filter(Boolean) };
    case "transcript":
      return { kind: "Text", facts: [] };
    default:
      return { kind: (p as Product).kind, facts: [] };
  }
}

/** A call's products, a line each (nothing when it has none). */
export function Products({ call }: { call: CallEntry }) {
  if (!call.products?.length) return null;
  return (
    <ul className="products">
      {call.products.map((p, i) => {
        const f = facts(p);
        return (
          <li key={i} title={`${p.kind} · ${p.by}`}>
            <span className="tag mono">{f.kind}</span>
            {f.facts.map((x, j) => (
              <span key={j} className="mono small muted">
                {x}
              </span>
            ))}
            <span className="product-text">{productText(p)}</span>
          </li>
        );
      })}
    </ul>
  );
}
