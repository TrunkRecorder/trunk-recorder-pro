// What plugins made of a call (its `products`: a transcript, decoded pages, ACARS blocks).
import type { CallEntry, Product } from "./protocol.ts";

/** `entries` with `p` on the call it's of (a `product` message). */
export function withProduct<T extends CallEntry>(entries: T[], p: Product): T[] {
  const { call: _call, ...own } = p as Product & { type?: string };
  delete (own as { type?: string }).type;
  return entries.map((c) => (c.record.transmission_id && c.record.transmission_id === p.transmission_id ? { ...c, products: [...(c.products ?? []), own as Product] } : c));
}

/** A product's text: what a list shows and a filter looks in. */
export const productText = (p: Product): string => (p.data as { text?: string } | undefined)?.text ?? "";

/** A call's products' text, for a filter. */
export const productsText = (c: CallEntry): string => (c.products ?? []).map(productText).join("\n");
