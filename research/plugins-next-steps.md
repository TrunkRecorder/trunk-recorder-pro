# Plugins: next steps

October 2026. Where the plugin system stands, and what's left before users
can install plugins from a store inside the app. The design of the registry
and the installer is in the template repo's
[docs/registry.md](../../trunk-plugin-template/docs/registry.md). This page is
the plan for building it.

**Constraint:** this repo (the app) stays private for now. Everything plugin
authors need has to be public without it: the SDK, the template and its docs,
the plugins and the registry.

## Where things stand

Done:

- **The protocol and the SDK**: `crates/trunk-recorder-plugin`.
- **The host**: `crates/trunk-pro/src/plugins/`. It runs plugins, restarts
  them, encodes M4A and has `trunk-pro plugin list|describe|run`.
- **The Plugins page**: an on/off switch for each plugin, and a settings form
  drawn from the plugin's schema. Changes apply live while recording.
- **Five plugins**, ported from Trunk Recorder, each in a sibling repo with
  tests and a release workflow: OpenMHz, Broadcastify Calls, Rdio Scanner,
  simplestream and upload-script.
- **The template repo**: a starter plugin plus docs for plugin authors.

Not done:

- **Nothing is published or pushed.** The SDK isn't on crates.io. The plugin
  repos and the template have no remote; four of the plugin repos have no
  commits.
- **The plugins can only be built on this machine.** They depend on the SDK
  through a gitignored `.cargo/config.toml` path patch, so their GitHub CI and
  release builds would fail.
- **The registry, the installer and the store don't exist yet.**

## 1. A public home for the SDK

The SDK is the public contract with plugin authors, so it should live in a
public repo of its own: `TrunkRecorder/trunk-recorder-plugin`.

- **Move the crate.** Move `crates/trunk-recorder-plugin` there with its
  history (`git subtree split --prefix crates/trunk-recorder-plugin`). Add a
  README and the `LICENSE-MIT` / `LICENSE-APACHE` texts, and set `repository`
  to the new repo.
- **Point this repo at it.** This repo depends on it from crates.io:
  `trunk-recorder-plugin = { version = "0.1", default-features = false }`.
- **Developing both at once.** When a change touches the protocol and the app
  together, a `[patch.crates-io]` path entry points at a local checkout of the
  SDK. That's the same arrangement the plugin repos use now.
- **The author docs could move there too.** They're the template's `docs/`
  folder now. In either place they're public, which is what matters.

Considered instead: publishing to crates.io straight from this private repo.
It works, since only the crate's files are uploaded. But the crates.io page
would have no public repo to link to, and authors would have nowhere to file
issues or send fixes.

## 2. Publish the SDK to crates.io

Publishing can't be undone: a version can be yanked, but not deleted.

1. Read through the crate's source once more: it becomes public.
2. Fill in the metadata: `readme`, `documentation`, `keywords` and
   `categories`. `cargo package` already builds it cleanly (12 files), and the
   name `trunk-recorder-plugin` is free.
3. On crates.io, sign in with GitHub, verify the email address, and make an
   API token with the `publish-new` and `publish-update` scopes. Then run
   `cargo login`.
4. Run `cargo publish --dry-run`, then `cargo publish`.
5. Add the TrunkRecorder GitHub team as an owner, so the crate isn't tied to
   one account: `cargo owner --add github:TrunkRecorder:<team>`.
6. Tag SDK releases `v0.1.0`, `v0.1.1`, … in the SDK repo. Crate versions
   follow semver. The protocol's `API_VERSION` changes only when an older
   recorder can't run a newer plugin.

## 3. Push the plugins and release them

For each of the five plugin repos and the template:

1. **Build against crates.io.** Delete `.cargo/config.toml`, then run
   `cargo update -p trunk-recorder-plugin` and `cargo test`. Commit
   `Cargo.lock`: CI builds with `--locked`.
2. **Fix the README links.** They point at `TrunkRecorder/trunk-recorder-lite`,
   which is private and still uses the product's old name. Point them at a
   public page for the app instead (a site or a releases repo), or leave the
   link out.
3. **Check what the workflows build.** The Release workflow builds for four
   platforms, and so far the plugins have only been built on this Mac. The
   first real release will show any cross-compile problems, so release one
   plugin and fix what breaks before releasing the rest.
4. **Push and tag `v0.1.0`.** The Release workflow publishes the archives,
   `<id>-<version>.manifest.json` and `SHA256SUMS`.

The template's docs still say "Trunk Recorder Lite" and `trunk-lite` in
places. Rename them to Pro before they go public.

## 4. The registry repo: `TrunkRecorder/plugins`

As designed in registry.md: one `plugins/<id>.json` per plugin. Each entry
pins one release: its version, its tag's commit, and every platform
archive's URL and SHA-256. Authors keep their plugins in their own repos and
send a PR to bump their entry when they release.

To build:

- **`add-release <repo> <tag>`.** It reads the release's `SHA256SUMS` and
  manifest and writes the entry, including the tag's commit SHA. Authors run
  it for their PRs.
- **PR checks.** Download every asset and check its checksum. Check that each
  URL is a release asset of the entry's repo. Run the Linux build's
  `--describe` and check that its id, version and `api` match the entry.
- **`index.json`.** CI builds one file of every entry from `plugins/*.json`,
  so the recorder fetches a single file.
- **Review.** A `CODEOWNERS` file and the review checklist in registry.md.
- **Later:** a workflow that watches listed repos for new tags and opens the
  bump PRs itself.

**Pin the commit, not just the tag.** The checksums stop anyone swapping an
archive after review, but a tag can be moved to another commit. With the
commit in the entry, it's always clear which source was reviewed.

**Add build provenance.** GitHub's `actions/attest-build-provenance` step in
the plugins' release workflow proves an archive was built by Actions from
that commit. Review currently takes that on trust.

**Seed it with the five plugins**, as `tier: "official"`.

## 5. The installer in the app

`trunk-pro` has no HTTP client, archive or hashing code yet. It needs ureq
(with rustls, so builds still cross-compile), `sha2`, `tar` + `flate2`, and
`zip` (for Windows).

- **The registry.** Fetch `index.json` from the registry repo. Fall back to a
  copy built into the app, so the store works offline.
- **Install.** Pick the archive for this platform and download it. Stop if its
  SHA-256 doesn't match. Unpack it into `plugins/<id>/` in the config folder.
  Run `--describe` and check its id and `api`. The Plugins page already finds
  plugins installed there.
- **Update and uninstall.** Settings are in `plugins.json`, so they carry
  over. While recording, the live host swap restarts the plugin.
- **The CLI first.** `trunk-pro plugin search|install|update|uninstall`. It's
  quick to build, and the UI then has something to call.

## 6. The store in the UI

A **Browse** section on the Plugins page:

- the registry's plugins, each with an official or community badge
- **Install**, **Installed** and **Update available** buttons
- the plugin's settings form, opened right after it's installed
- (later) install from a GitHub release URL, with a warning that the plugin
  wasn't reviewed

## Open questions

- **upload-script built in?** Users coming from Trunk Recorder expect
  `uploadScript` to just work, so it could ship with the app instead of being
  installed from the store.
- **Old versions in the registry?** Keep only the latest version of each
  plugin (simpler), or older ones too, so users can roll back?
- **How users find the app.** Plugin READMEs and the registry need a public
  page to link to while this repo is private.
- **Who reviews community plugins**, and how quickly.

## Order

1. SDK repo and crates.io (sections 1–2). Nothing else can be built or
   released on GitHub until this is done.
2. Release one plugin end to end, fix the workflows, then release the rest
   (section 3).
3. The registry repo and its checks (section 4).
4. The installer, CLI first (section 5).
5. The store UI (section 6).
