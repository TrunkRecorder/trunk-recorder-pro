# Docker

This page covers running Trunk Recorder Pro in a Docker container on a Linux host: the image, the
`docker-compose.yml` that comes with it, USB passthrough, and what the container can and can't do.

## Before you start

- **A Linux host**, x86-64 or ARM64 (a Raspberry Pi with a 64-bit OS works). The image is built
  for `linux/amd64` and `linux/arm64`.
- **Not Docker Desktop on macOS or Windows.** Containers there run inside a virtual machine that
  can't see your USB devices, so the recorder would find no dongles. Use the [macOS](macos.md) or
  [Windows](windows.md) app instead.
- **RTL-SDRs only.** The image contains `trunk-pro` and a small `ffmpeg`, nothing else: no UHD,
  libairspy or SoapySDR, so USRP, Airspy and SoapySDR sources (and older RTL-SDRs that need
  SoapySDR) don't work in it. Use the [Linux](linux.md) package for those.
- **Nothing else may hold the dongles.** Stop any SDR program on the host that has them open. The
  kernel's DVB TV driver is not a problem: Trunk Recorder Pro detaches it itself.

## The image

The image is on Docker Hub as
[`robotastic/trunk-recorder-pro`](https://hub.docker.com/r/robotastic/trunk-recorder-pro):

| Tag | What it is |
|---|---|
| `latest` | The newest release |
| `1.2.3`, `1.2` | A specific release, or the newest of a minor version (a pre-release such as `1.2.3-rc.1` gets only its own tag) |
| `edge` | Built from every change to the main branch: newest, least tested |

By default the container runs `trunk-pro serve --no-open --bind 0.0.0.0`: the recorder with its
interface listening on port 8080 on every address inside the container.

## Run it with Docker Compose

Get [`docker-compose.yml`](../../docker-compose.yml) from the repository, put it in a folder of its
own (say `~/trunk-pro`), and from that folder:

```bash
docker compose up -d
```

Then open `http://<host>:8080` and set it up there, starting at
[Getting started](../getting-started.md). The interface is the same as on the desktop.

The compose file:

```yaml
services:
  trunk-pro:
    image: robotastic/trunk-recorder-pro:latest
    build: .
    container_name: trunk-pro
    restart: unless-stopped
    ports:
      - "8080:8080"
    volumes:
      - ./data:/data
      - /dev/bus/usb:/dev/bus/usb
      - /etc/localtime:/etc/localtime:ro
    device_cgroup_rules:
      - "c 189:* rmw"
```

What each part does:

| Setting | Why |
|---|---|
| `image` | The Docker Hub image. `build: .` lets `docker compose up -d --build` build it from a checkout of the repository instead. |
| `restart: unless-stopped` | Starts it again after a crash or a reboot of the host, unless you stopped it yourself. |
| `ports: "8080:8080"` | The interface, on port 8080 of every address of the host. |
| `./data:/data` | Everything the recorder keeps: settings, band plans, plugins and recordings, in `./data` next to the compose file. |
| `/dev/bus/usb:/dev/bus/usb` | The host's USB devices, including ones plugged in after the container starts. |
| `device_cgroup_rules: "c 189:* rmw"` | Permission to open USB devices (character devices with major number 189). Together with the line above, this lets the container use any dongle without `privileged: true`. |
| `/etc/localtime` | The host's time zone, so call times and file names are local time. |

### Start recording on its own

The container starts the recorder, but recording waits for **Start** until you either turn on
**Start recording when the app starts** in **Setup → Recording** (recommended: set it once and
every restart resumes recording), or uncomment the `command:` line in the compose file:

```yaml
    command: ["serve", "--no-open", "--bind", "0.0.0.0", "--start"]
```

## Where the files are

Inside the container everything is under `/data`, which the compose file maps to `./data` on the
host:

| What | Host | Container |
|---|---|---|
| Settings (`config.json`), band plans, plugins, statistics | `./data/config/trunk-pro/` | `/data/config/trunk-pro/` |
| Recordings | `./data/TrunkRecorderPro/` | `/data/TrunkRecorderPro/` |

The container runs as root, so the files in `./data` belong to root on the host. Use `sudo` to
edit or remove them.

If you change the **Recordings folder** in the settings, keep it under `/data` (for example
`/data/recordings`), or mount another host folder into the container and point it there.
Anything written elsewhere in the container is lost when the container is replaced.

## Security

> **The interface has no login.** Anyone who can reach port 8080 can change the settings, install
> plugins and stop the recorder. Only publish the port on a network you trust.

To keep the interface to the host itself, publish it on localhost only:

```yaml
    ports:
      - "127.0.0.1:8080:8080"
```

and reach it from elsewhere through an SSH tunnel (`ssh -L 8080:localhost:8080 you@host`, then
`http://localhost:8080`). To use another port on the host, change only the left-hand number:
`"8081:8080"`.

## Everyday commands

```bash
docker compose logs -f              # follow the log
docker compose restart              # restart the recorder
docker compose pull && docker compose up -d     # update to the newest image
docker compose down                 # stop and remove the container (./data is kept)
docker compose exec trunk-pro trunk-pro devices # list the dongles the container sees
```

When the container stops, Docker sends it SIGTERM and Trunk Recorder Pro saves the calls in
progress before it exits.

## Without Compose

The same thing with `docker run`:

```bash
docker run -d --name trunk-pro --restart unless-stopped \
  -p 8080:8080 \
  -v "$PWD/data:/data" \
  -v /dev/bus/usb:/dev/bus/usb \
  -v /etc/localtime:/etc/localtime:ro \
  --device-cgroup-rule 'c 189:* rmw' \
  robotastic/trunk-recorder-pro:latest
```

## Building the image yourself

From a checkout of the repository:

```bash
docker build -t trunk-pro .
docker buildx build --platform linux/amd64,linux/arm64 -t trunk-pro .    # both architectures
```

or `docker compose up -d --build`.

## Problems

- **No dongles in Setup.** Check the host sees them (`lsusb`), that no program on the host has
  them open, and that the compose file's `/dev/bus/usb` volume and `device_cgroup_rules` are in
  place. `docker compose exec trunk-pro trunk-pro devices` shows what the container sees.
- **The page doesn't load from another machine.** Check the `ports:` line (a `127.0.0.1:` prefix
  limits it to the host) and the host's firewall.
- **Call times are in UTC.** The `/etc/localtime` volume is missing.

The **Platform** page in the interface knows it's in a container: its CPU and memory figures are
the container's limits, and its disks are the mounted volumes.
