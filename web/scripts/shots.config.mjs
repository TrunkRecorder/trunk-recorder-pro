// The docs' screenshots: which screens, and how each is reached (npm run shots;
// docs/developer/README.md → Screenshots). Each shot is written as
// docs/img/shots/<name>-light.png and <name>-dark.png.
//
// A shot:
//   name      the file's stem
//   scenario  the recorder it's taken on (below)
//   live      true: taken while the replay runs, `after` seconds in (default 35)
//   url       the page's address (#/radio/dcfd), or a path (/signin)
//   storage   localStorage set before the page loads (the Setup tab, cards open)
//   prepare   steps once it's loaded, in order: { tab: "Storage" } clicks the tab
//             saying so, { click: "Add a system" } a button or link, { wait: js }
//             waits for an expression, { eval: js } runs one
//   ready     an expression that must be true before the picture
//   settle    ms to wait on the page first (canvases filling)
//   clip      a CSS selector, or { card: "Access" }: the card or section with
//             that heading. None: the window
//   width, height   the window, when 1440 × 900 isn't it
//   redact    { selector: text }: text replaced before the picture

/** The systems of the corpus captures the replay scenario plays (captures/regress.sh has the same). */
const dcfd = { shortName: "dcfd", name: "DC Fire and EMS", type: "p25", sites: [{ controlChannelsHz: [857987500, 855462500] }] };
const wmata = {
  shortName: "wmata",
  name: "WMATA",
  type: "smartnet",
  defaultMode: "analog",
  sites: [{ controlChannelsHz: [496437500, 490862500, 496537500, 490787500], bandplan: "400_custom", bandplanBaseHz: 489087500, bandplanSpacingHz: 25000, bandplanOffset: 380, bandplanHighHz: 496612500 }],
};
// (Its channels are in the band the second WMATA capture covers, between that system's own.)
const ops = {
  shortName: "ops",
  name: "Building operations",
  channels: [
    { freqHz: 489950000, mode: "fm", name: "Facilities", talkgroup: 1 },
    { freqHz: 490100000, mode: "fm", name: "Security", talkgroup: 2 },
    { freqHz: 491150000, mode: "fm", name: "Events", talkgroup: 3 },
  ],
};

/**
 * The recorders. `captures`: corpus captures played as its radios, each source `[capture, file, centerHz]`
 * (trunk-pro tool corpus fetch --only phase2,smartnet). `users`: sign-in on, with these.
 */
export const scenarios = {
  // Nothing set up: the setup guide offers itself.
  empty: { config: {} },
  // Two systems recording: stopped first (Setup, Plugins, Interfaces), then replaying.
  replay: {
    config: { systems: [dcfd, wmata], conventional: [ops] },
    captures: [
      ["phase2", "source-0.sigmf-data", 858300000],
      ["phase2", "source-1.sigmf-data", 770400000],
      ["smartnet", "source-0.sigmf-data", 496337000],
      ["smartnet", "source-1.sigmf-data", 490600000],
    ],
    talkgroups: true,
  },
  signin: { config: { systems: [dcfd] }, users: [["alex", "control"], ["kiosk", "listen"]] },
};

const setup = (tab) => ({ scenario: "replay", url: "#/setup", storage: { "trp.setupTab": tab }, ready: `document.querySelector(".setup-tabs [aria-selected=true]")?.textContent.includes(${JSON.stringify(tab[0].toUpperCase() + tab.slice(1))})` });

export default [
  { name: "setup-guide", scenario: "empty", url: "#/overview", ready: `!!document.querySelector(".guide, [role=dialog]")`, settle: 500 },

  { name: "setup-systems", ...setup("systems") },
  { name: "setup-conventional", ...setup("conventional") },
  { name: "setup-radios", ...setup("radios") },
  { name: "setup-recording", ...setup("recording"), height: 1300 },
  { name: "setup-data", ...setup("data"), height: 1300 },
  { name: "setup-plugins", ...setup("plugins") },
  { name: "plugins-install", scenario: "replay", url: "#/plugins/manage", ready: `[...document.querySelectorAll("button")].some((b) => b.textContent.trim() === "Install")`, settle: 500 },
  { name: "interfaces", scenario: "replay", url: "#/interfaces", settle: 1500 },

  { name: "listen", scenario: "replay", live: true, url: "#/listen", settle: 1000 },
  { name: "decode", scenario: "replay", live: true, url: "#/decode", settle: 2000 },
  { name: "rf", scenario: "replay", live: true, url: "#/rf", settle: 6000, height: 1200 },
  { name: "platform", scenario: "replay", live: true, url: "#/platform", settle: 1500, height: 1200 },
  { name: "platform-storage", scenario: "replay", live: true, url: "#/platform", prepare: [{ tab: "Storage" }], settle: 1500, height: 1200 },
  { name: "overview", scenario: "replay", live: true, after: 50, url: "#/overview", settle: 1500 },
  { name: "radio-system", scenario: "replay", live: true, after: 55, url: "#/radio/dcfd", ready: `document.querySelectorAll("main table tbody tr").length >= 3`, settle: 1500, height: 1200 },

  { name: "signin", scenario: "signin", signedOut: true, url: "/signin", ready: `document.title === "Sign in"`, width: 900, height: 600 },
  { name: "setup-access", scenario: "signin", url: "#/setup", storage: { "trp.setupTab": "recording" }, clip: { card: "Access" }, height: 1300 },
];
