// Where you are, roughly, on a map: a small circle (5 km across) stays in the
// middle while you move the map under it. Its centre is the location the
// license lookup uses: it needs only your general area, which the circle shows.
// The map (MapLibre, loaded only when this opens) is the systems directory's:
// its vector tiles, fonts and style come from trunkrecorder.pro (server.trpApi).

import { useEffect, useRef, useState } from "react";
import type { Map as MlMap } from "maplibre-gl";

/** The circle's radius: about how well the lookup needs to know where you are. */
export const CIRCLE_KM = 5;
const US = { center: [-97.5, 39.5] as [number, number], zoom: 3.2 };

const light = () => typeof matchMedia !== "undefined" && matchMedia("(prefers-color-scheme: light)").matches;

/** Metres a screen pixel covers at `lat`, zoom `z` (MapLibre's 512 px tiles). */
const metresPerPx = (lat: number, z: number) => (40_075_016.686 * Math.cos((lat * Math.PI) / 180)) / (512 * 2 ** z);

export function LocationMap(props: { base: string; at: [number, number] | null; onMove: (at: [number, number]) => void }) {
  const el = useRef<HTMLDivElement>(null);
  const map = useRef<MlMap | null>(null);
  const onMove = useRef(props.onMove);
  onMove.current = props.onMove;
  const [px, setPx] = useState(14);
  const [failed, setFailed] = useState(false);
  const moving = useRef(false);

  useEffect(() => {
    let gone = false;
    (async () => {
      const ml = await import("maplibre-gl");
      // Its worker, as a file of the build (MapLibre finds it by a URL made at run time, which a bundler can't see).
      ml.setWorkerUrl((await import("maplibre-gl/dist/maplibre-gl-worker.mjs?url")).default);
      await import("maplibre-gl/dist/maplibre-gl.css");
      if (gone || !el.current) return;
      const at = props.at;
      const m = new ml.Map({
        container: el.current,
        style: `${props.base}/directory/map/style-${light() ? "light" : "dark"}.json`,
        center: at ? [at[1], at[0]] : US.center,
        zoom: at ? 9 : US.zoom,
        maxZoom: 12.5,
        dragRotate: false,
        pitchWithRotate: false,
        attributionControl: { compact: true },
      });
      m.addControl(new ml.NavigationControl({ showCompass: false }), "top-right");
      m.touchZoomRotate.disableRotation();
      map.current = m;
      const size = () => {
        const c = m.getCenter();
        setPx(Math.max(14, (2 * CIRCLE_KM * 1000) / metresPerPx(c.lat, m.getZoom())));
      };
      m.on("move", size);
      m.on("load", size);
      m.on("moveend", () => {
        const c = m.getCenter();
        moving.current = true;
        onMove.current([Math.round(c.lat * 1e4) / 1e4, Math.round(c.lng * 1e4) / 1e4]);
      });
      // A click: the circle goes there.
      m.on("click", (e: { lngLat: { lat: number; lng: number } }) => m.easeTo({ center: [e.lngLat.lng, e.lngLat.lat], zoom: Math.max(m.getZoom(), 8) }));
      m.on("error", (e: { error?: { message?: string } }) => {
        if (/style|tiles|Failed to fetch|NetworkError|AJAXError/i.test(String(e.error?.message ?? ""))) setFailed(true);
      });
    })().catch(() => setFailed(true));
    return () => {
      gone = true;
      map.current?.remove();
      map.current = null;
    };
  }, [props.base]);

  // A location from elsewhere (yours, a place typed): the map goes there.
  useEffect(() => {
    const m = map.current;
    if (!m || !props.at) return;
    if (moving.current) return void (moving.current = false);
    const c = m.getCenter();
    if (Math.abs(c.lat - props.at[0]) > 1e-3 || Math.abs(c.lng - props.at[1]) > 1e-3) m.easeTo({ center: [props.at[1], props.at[0]], zoom: Math.max(m.getZoom(), 9) });
  }, [props.at?.[0], props.at?.[1]]);

  if (failed) return <p className="muted small">The map isn't available right now: use your location, or type a place or coordinates.</p>;
  return (
    <div className="location-map" role="region" aria-label="Map: move it so the circle is around where you are">
      <div ref={el} className="location-map-canvas" />
      <div className="location-circle" style={{ width: px, height: px }} aria-hidden="true" />
    </div>
  );
}
