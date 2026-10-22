# Interfaces

Trunk Recorder Pro's own web interface is for whoever runs the recorder: it sets up radios and
systems, starts and stops recording, installs plugins. People who only want to **listen** need
something else: a player, a scanner wall for a dispatch room, a phone page. Those are
**interfaces**, and the recorder serves as many of them as you like, beside its own.

- An interface is a web page: HTML, JavaScript and CSS, with no server side. It runs in the
  browser and talks to the recorder over the same [API](../api/README.md) its own interface uses.
- Each one is served on **a port of its own**: the recorder's interface on 8080, a player on 8081,
  a wall display on 8082. To a browser each port is a separate site, so interfaces can't read each
  other's settings or the recorder interface's.
- Each one gets an **access**: **listen** (the default) lets it see everything the recorder does
  and play its calls, and change nothing. **Control** lets it do everything the recorder's own
  interface can.

Interfaces come from the **interface store** (the
[TrunkRecorder/interfaces](https://github.com/TrunkRecorder/interfaces) registry), from a GitHub
release, or from a folder of your own.

## The Interfaces page

In the desktop app, **Interfaces** in the list of pages on the left. It has four parts:

**Interfaces**: each interface the recorder serves, with:

- its name, version, who made it (the GitHub account it comes from), and its screenshots — click
  one to see it large;
- where it's from: **Official** or **Community** (from the store, reviewed),
  **Not reviewed** (installed from a GitHub release), **your folder**, or **this run only**
  (`--ui`);
- its state: **Serving on port 8081**, **Off**, or **Not served** with the reason (its port is
  taken, its folder has no `index.html`…);
- **On** / **Off**: whether it's served. Off keeps it, and its port, for later;
- **Open**: the interface, in a new tab;
- **Access**: **Listen** or **Control**. Changing to Control asks first. An interface the store
  lists as needing control shows **(it asks for control)** while it only has Listen;
- **Update to v…** when the store has a newer version;
- **Uninstall** (an installed one: its files are deleted) or **Remove** (a folder of your own:
  the folder is left alone).

**Add a folder…** serves a folder on the recorder's computer, at once, on the next free port.

**Ports**: the recorder's own interface's port, and each interface's. Type a port and press Enter
(or leave the field): the interface moves there at once, and pages open on the old port lose
their connection. The recorder interface's own port changes when the app restarts. A port can't
be another interface's or the recorder interface's; one below 1024 usually needs administrator
rights and fails with a note saying so.

**Find interfaces**: the store, each with its screenshots, so you can see what it looks like
before installing it. **Install** downloads the interface, checks it against the
checksum the store pinned, and serves it on the next free port, with the access it asks for.
One that asks for control says so, and asks you before installing. **Install from GitHub…**
installs a release of an interface that isn't in the store; nobody has reviewed it, so it may
only listen until you give it control.

**Pages served elsewhere**: web pages served by something else (a development server, another
machine) that may use the recorder interface's API, with control. An interface the recorder
serves needs none of this.

Everything here is saved in the config file (`server.interfaces`) as you change it and takes
effect at once: no restart. See [server.interfaces](../configuration/server-and-log.md#interfaces).

## Reaching interfaces from other computers

By default the recorder listens on `127.0.0.1`: every interface, its own included, is reachable
from this computer only. To reach them from your network, set `server.bind` to `0.0.0.0` (or
start with `--bind 0.0.0.0`), and let the interfaces' ports through the computer's firewall.

That reaches the recorder's own interface too, and **it has no login**. To share the recorder
with listeners without handing them the controls, give them a listen-access interface's address
(`http://recorder.local:8081/`) and keep the recorder's own port (8080) closed to them: on the
firewall, or with an SSH tunnel for yourself ([Remote access](../guides/interface.md#remote-access)).
A listen-access interface can't change anything, and its connection can't reach the recorder's
port to borrow its access: the recorder refuses a page from another port.

## What listen access can and can't do

| | Listen | Control |
|---|:---:|:---:|
| See status, systems, calls on the air, recorded calls, the dashboard's figures | ✓ | ✓ |
| Play live audio and recorded calls | ✓ | ✓ |
| See the config | without plugin settings and key sources | ✓ |
| Change settings, start and stop recording, quit | | ✓ |
| Install, set up or remove plugins and interfaces | | ✓ |
| Unlock encryption keys, browse the recorder's folders, scan for systems | | ✓ |

## From the command line

On a recorder with no browser nearby:

```bash
trunk-pro interface search                # what the store has
trunk-pro interface install trunkamp      # install one, on the next free port
trunk-pro interface add ~/wall --port 8090
trunk-pro interface list                  # ports, access, addresses
```

See [Command line](../command-line.md#interface). A running recorder serves what these change after
a restart.

For one run, `trunk-pro --ui <folder>` serves a folder as an interface (with control access, on
the next free port) and opens it, without saving anything: handy while building one.

## Building your own

An interface is a folder with an `index.html`. [The API guide](../api/README.md) covers the
messages, the JavaScript client the recorder serves at `/api/client.js`, and examples; a running
recorder serves it at `/api/docs`. To publish yours in the store, see
[Publishing an interface](publishing.md).
