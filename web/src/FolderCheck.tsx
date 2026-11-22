// Whether the recorder can write to a folder the settings name: the
// recorder's own check (crate::folder), as the `host` message last had it.
// Shown once the folder in the settings is the one checked (an edit is
// checked within a couple of seconds of being saved).

import { Info } from "./charts.tsx";
import { useApp } from "./controller.ts";
import { folderWhy } from "./dash/data.ts";

/** The folder `name` ("recordings") at `path`: writable, or why not; nothing until it's been checked there. */
export function FolderCheck({ name, path }: { name: string; path: string }) {
  const f = useApp().host?.platform.folders?.find((x) => x.name === name && x.path === path);
  if (!f || f.ok === null) return null;
  if (f.ok) return <span className="folder-check ok">Writable</span>;
  return (
    // (Inside a field's label: a click on the ⓘ isn't one on the input.)
    <span className="folder-check bad" onClick={(e) => e.preventDefault()}>
      Can't write: {f.why}
      <Info label="Why">{folderWhy(f)}</Info>
    </span>
  );
}
