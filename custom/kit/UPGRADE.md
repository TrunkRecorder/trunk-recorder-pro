# Trunk Recorder Pro 0.5.0-custom-v39: upgrade steps

v39 = official **0.5.0** plus our changes, all carried over unchanged in what they do: the EDACS
data-burst fix, SoapySDR Find keeping the RSPduo's tuner mode, SoapySDR live AGC / gain stages,
the multi-site fixes, the EDACS tail trim, Setup's Live / Next Start marks and Find list, and the
page-tone edge fix from v37. New in v39: on a plugin's own page, its Settings card goes above
its lists when the plugin orders its settings first (Page alerts 0.5.3 does), so the settings
and buttons aren't below hundreds of tone sets. Coming from v38: nothing else changes, and
steps 1 and 7 can be skipped.

## Before you upgrade

- **The recordings folder must be a full path** (Setup → Recording → recordings folder), such as
  `/home/rdioa/TrunkRecorderPro` or `~/TrunkRecorderPro`. A relative one (`recordings`) now stops
  Start with a message. Check each box first:

      grep -o '"captureDir": *"[^"]*"' ~/.config/trunk-pro/config.json

- Plugins keep working as they are (Page alerts, alert health, Rdio Scanner, …): the plugin
  protocol is unchanged. Update any that offer an update afterwards.
- Coming from v36 / v37 (0.4.1): nothing changes in the store. Coming from older (multi, on
  0.2.5): also do the plugin updates in step 7, and keep the backup from step 1 a while.

## Steps (on each box)

1. Back up the config folder:

       cp -a ~/.config/trunk-pro ~/trunk-pro-config-backup-$(date +%F)

2. Unpack:

       cd ~/Downloads
       rm -rf trunk-pro-0.5.0-custom-v39-build-kit
       tar xzf trunk-pro-0.5.0-custom-v39-build-kit.tar.gz
       cd trunk-pro-0.5.0-custom-v39-build-kit

3. Build (10-20 minutes; recording keeps running). The last line must say
   `trunk-pro 0.5.0-custom-v39`:

       bash build.sh

4. Stop, back up the program, install (from this same folder):

       B=$(command -v trunk-pro)
       systemctl --user stop trunk-pro
       sudo cp "$B" "$B.prev"
       sudo install -m 755 trunk-pro-0.5.0-custom "$B"
       trunk-pro --version

5. Real-time priority for the radios (repeat after every update):

       sudo setcap cap_sys_nice+ep "$B"

6. Start it (on rdioa allow a minute: the 30 s boot delay):

       systemctl --user start trunk-pro
       sleep 60
       systemctl --user is-active trunk-pro

   It must print `active`. Hard-refresh the page (Ctrl+Shift+R): the top-left says
   **v0.5.0-custom-v39**.

7. Plugins page: press **Update to v…** on any plugin that offers it.

## Going back

    B=$(command -v trunk-pro)
    systemctl --user stop trunk-pro
    sudo install -m 755 "$B.prev" "$B"
    mv ~/.config/trunk-pro ~/trunk-pro-config-0.5.0-$(date +%F)
    cp -a ~/trunk-pro-config-backup-<date> ~/.config/trunk-pro
    systemctl --user start trunk-pro
