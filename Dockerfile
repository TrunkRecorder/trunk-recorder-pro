# Trunk Recorder Pro, headless: the app and its browser interface, for a
# Linux machine with SDRs plugged in. See docker-compose.yml to run it.
#
#   docker build -t trunk-pro .
#   docker buildx build --platform linux/amd64,linux/arm64 -t trunk-pro .
#
# Five stages: the config's rules as WebAssembly (the interface uses the
# recorder's own), the web interface (embedded in the binary), the app, a small
# static ffmpeg that only makes M4A from WAV (for plugins that upload
# compressed audio), and the image itself — Debian slim, since plugins are
# downloaded as glibc builds and upload-script runs /bin/sh scripts.

ARG DEBIAN=trixie

# --- The interface's WebAssembly (trunk-web) → /pkg ---------------------------
FROM --platform=$BUILDPLATFORM rust:1-slim-${DEBIAN} AS wasm
# Must match the wasm-bindgen version in Cargo.lock (as CI's).
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    rustup target add wasm32-unknown-unknown && cargo install wasm-bindgen-cli --version 0.2.129 --locked
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo/ .cargo/
COPY crates/ crates/
COPY docs/api/ docs/api/
COPY packaging/icons/ packaging/icons/
COPY web/src/protocol.ts web/src/protocol.ts
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked -p trunk-web --target wasm32-unknown-unknown --profile dist \
    && wasm-bindgen --target web --out-dir /pkg --out-name trunk_web target/wasm32-unknown-unknown/dist/trunk_web.wasm

# --- The browser interface → web/dist ----------------------------------------
FROM --platform=$BUILDPLATFORM node:22-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY Cargo.toml /src/
# The live-audio player is client.js's, the one served to every interface.
COPY docs/api/client.js /src/docs/api/client.js
COPY web/ ./
COPY --from=wasm /pkg src/web/pkg
RUN npm run build

# --- The app ------------------------------------------------------------------
FROM rust:1-slim-${DEBIAN} AS app
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY docs/api/ docs/api/
COPY packaging/icons/ packaging/icons/
COPY web/src/protocol.ts web/src/protocol.ts
COPY --from=web /src/web/dist web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked -p trunk-pro --profile dist \
    && cp target/dist/trunk-pro /usr/local/bin/trunk-pro

# --- ffmpeg: WAV in, AAC in MP4 out, nothing else (~2 MB, static) ----------
FROM debian:${DEBIAN}-slim AS ffmpeg
ARG FFMPEG=7.1.2
RUN apt-get update && apt-get install -y --no-install-recommends gcc libc6-dev make curl ca-certificates xz-utils \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
RUN curl -fsSL https://ffmpeg.org/releases/ffmpeg-${FFMPEG}.tar.xz | tar -xJ --strip-components=1
RUN ./configure --prefix=/opt/ffmpeg \
      --enable-static --disable-shared --extra-ldflags=-static --pkg-config-flags=--static \
      --disable-autodetect --disable-everything --disable-doc --disable-network \
      --disable-programs --enable-ffmpeg --disable-ffplay --disable-ffprobe \
      --disable-swscale --disable-x86asm --enable-small \
      --enable-protocol=file,pipe \
      --enable-demuxer=wav --enable-decoder=pcm_s16le,pcm_f32le \
      --enable-encoder=aac --enable-muxer=mp4 \
      --enable-filter=aresample,aformat,anull,atrim,abuffer,abuffersink \
    && make -j"$(nproc)" ffmpeg && strip ffmpeg \
    && ./ffmpeg -hide_banner -version | head -1

# --- The image ----------------------------------------------------------------
FROM debian:${DEBIAN}-slim
COPY --from=ffmpeg /src/ffmpeg /usr/local/bin/ffmpeg
COPY --from=app /usr/local/bin/trunk-pro /usr/local/bin/trunk-pro
# Everything the app writes is under /data: settings, band plans and plugins
# in /data/config/trunk-pro, recordings in /data/TrunkRecorderPro.
ENV HOME=/data XDG_CONFIG_HOME=/data/config
RUN mkdir -p /data/config/trunk-pro /data/TrunkRecorderPro
WORKDIR /data
VOLUME /data
EXPOSE 8080
# Reachable from outside the container: there is no login, so only publish
# the port on a network you trust.
ENTRYPOINT ["trunk-pro"]
CMD ["serve", "--no-open", "--bind", "0.0.0.0"]
