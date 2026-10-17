// What the browser gives the app and Node doesn't, enough for a test to
// import the controller and render a page: no recorder to connect to.
// Import it before anything of the app's.

const g = globalThis as Record<string, unknown>;
g.location ??= { protocol: "http:", host: "localhost", hash: "", pathname: "/", search: "" };
g.WebSocket = class {
  binaryType = "";
  onopen: (() => void) | null = null;
  onmessage: (() => void) | null = null;
  onclose: (() => void) | null = null;
  send(): void {}
  close(): void {}
};
g.addEventListener ??= () => {};
g.removeEventListener ??= () => {};
g.history ??= { pushState() {}, replaceState() {} };
g.scrollTo ??= () => {};
