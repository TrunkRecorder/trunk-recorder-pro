//! The config as a listen-only page is sent it ([`listen_config`]): what the
//! pages a View user has show (the radios, the systems and their channels,
//! how calls are recorded, what's watched), and nothing that's a secret, a
//! way in, or where things are on the machine.
//!
//! Every field of every struct here is named as shown or withheld
//! ([`shown!`]): one added to the config doesn't compile until it's
//! decided, so a new secret is never sent because nobody thought of it.
//! What's withheld is sent as its default.

use trunk_app::config::{Channel, Config, Conventional, Diagnostics, LogSettings, MonitorSettings, RamSpool, Recording, Server, SignalDef, Site, Source, StorageSettings, System};

/// `$v` (a `$T`) with the fields listed first as they are and those
/// `withheld` as their defaults; a field in neither list is a compile error.
macro_rules! shown {
    ($v:expr, $T:ident { $($keep:ident),* $(,)? }) => {{
        let $T { $($keep,)* } = $v;
        $T { $($keep,)* }
    }};
    ($v:expr, $T:ident { $($keep:ident),* $(,)? } withheld { $($drop:ident),* $(,)? }) => {{
        let $T { $($keep,)* $($drop: _,)* } = $v;
        $T { $($keep,)* ..Default::default() }
    }};
}

/// See the module.
pub fn listen_config(c: &Config) -> Config {
    // Withheld: plugin settings (API keys) and what's sent when, key
    // sources, and what the config has that the recorder doesn't know.
    let mut c = shown!(c.clone(), Config { sources, systems, conventional, next_system_id, recording, server, log, storage, monitor, diagnostics }
        withheld { plugins, plugin_delays, keys, other, notes, numbered });
    c.sources = c.sources.into_iter().map(|s| shown!(s, Source { kind, center_hz, rate_hz, ppm, auto_tune, guard_hz, name })).collect();
    c.systems = c.systems.into_iter().map(system).collect();
    c.conventional = c.conventional.into_iter().map(conventional).collect();
    c.recording = recording(c.recording);
    // Withheld: how the recorder is reached and by whom (addresses, other
    // interfaces' ports, origins, the users, the certificate's files).
    c.server = shown!(c.server, Server { port, auto_start, trp_api, hostname } withheld { bind, allowed_origins, interfaces, auth, tls, other });
    c.log = shown!(c.log, LogSettings { level, console, file, syslog_friendly, syslog, color, frequency_format, talkgroup_display_format, status_as_string, control_warn_rate_per_s }
        withheld { dir, other });
    // The database's address can hold its password.
    c.storage = shown!(c.storage, StorageSettings { backend, low_write, retention_days, commit_seconds, stats_days, radio_index, registry_radios, registry_days, talkgroup_index }
        withheld { url });
    c.monitor = shown!(c.monitor, MonitorSettings { check_hosts, check_every_s, network_checks, time_server, clock_warn_ms, check_updates });
    c.diagnostics = shown!(c.diagnostics, Diagnostics { rules, budget } withheld { capture_dir });
    c
}

fn system(s: System) -> System {
    let mut s = shown!(s, System { id, short_name, name, receive_on, fcc, share, directory, kind, enabled, unit_names, recording, default_mode, nxdn_type, signals, preferred_site, sites }
        withheld { keys, plugins, plugin_delays, other });
    s.signals = s.signals.into_iter().map(signal).collect();
    s.sites = s.sites.into_iter().map(|x| shown!(x, Site { name, directory_id, enabled, control_channels_hz, modulation, expect, voice_channels_hz, bandplan, bandplan_base_hz, bandplan_spacing_hz, bandplan_offset, bandplan_high_hz, lcn_table_hz, dmr_channels_hz, color_code, nxdn_rate, nxdn_channels_hz, ran, esk }
        withheld { other })).collect();
    s
}

fn conventional(v: Conventional) -> Conventional {
    let mut v = shown!(v, Conventional { id, short_name, name, fcc, enabled, squelch_db, demod, channels, signals, recording, unit_names }
        withheld { channel_file, channel_file_status, plugins, plugin_delays, keys, other });
    v.channels = v.channels.into_iter().map(|x| shown!(x, Channel { freq_hz, mode, name, talkgroup, code, slot, match_talkgroup, description, tag, group, squelch_db, bandwidth_khz, enabled, receive_on, auto_tune })).collect();
    v.signals = v.signals.into_iter().map(signal).collect();
    v
}

fn signal(d: SignalDef) -> SignalDef {
    shown!(d, SignalDef { id, name, pattern, tones_hz, digits, knox, lengths_s, extra_tolerance_pct, talkgroups } withheld { other })
}

fn recording(r: Recording) -> Recording {
    let mut r = shown!(r, Recording { call_timeout_s, conversation_mode, record_unknown, record_encrypted, record_unit_to_unit, keep_silent_calls, min_call_s, max_call_s, min_transmission_s, normalize_audio, digital_level_db, analog_level_db, archive_wav, archive_m4a, call_log, archive_files_on_failure, filename_format, preroll_s, max_recorders, capture_frames, drop_duplicate_calls, vocoder, m4a, ram_spool }
        withheld { capture_dir, other });
    r.ram_spool = shown!(r.ram_spool, RamSpool { enabled, size_mb } withheld { dir });
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every field that's a secret, a way in or a place on the machine,
    /// marked: none of them is sent, and what the pages show still is.
    #[test]
    fn listen_is_sent_what_its_pages_show_and_no_more() {
        let full: Config = serde_json::from_value(json!({
            "sources": [{ "type": "rtlsdr", "serial": "201", "centerHz": 855e6, "rateHz": 2.4e6 }],
            "plugins": { "openmhz": { "enabled": true, "settings": { "apiKey": "MARK-plugin" } } },
            "pluginDelays": [{ "plugins": ["MARK-delay"], "delayS": 30 }],
            "keys": { "a": { "file": "MARK-keys.json", "passwordFile": "MARK-pass" } },
            "markUnknown": "MARK-other",
            "systems": [{ "shortName": "x", "type": "p25", "keys": "MARK-syskeys", "plugins": { "openmhz": { "apiKey": "MARK-sysplugin" } }, "markSys": "MARK-sysother", "pluginDelays": [{ "plugins": ["MARK-sysdelay"], "delayS": 5 }],
                          "sites": [{ "name": "one", "controlChannelsHz": [855e6], "markSite": "MARK-siteother" }] }],
            "conventional": [{ "shortName": "c", "channelFile": "/MARK-channels.csv", "plugins": { "p": { "k": "MARK-convplugin" } }, "keys": "MARK-convkeys",
                               "channels": [{ "freqHz": 155e6, "name": "Fire" }] }],
            "recording": { "captureDir": "/MARK-recordings", "ramSpool": { "enabled": true, "dir": "/MARK-spool" }, "markRec": "MARK-recother" },
            "server": { "port": 8080, "bind": "MARK-bind", "allowedOrigins": ["https://MARK-origin"], "interfaces": { "MARK-interface": { "port": 9000 } }, "hostname": "radio.example",
                        "tls": { "cert": "/MARK-cert.pem", "key": "/MARK-key.pem" },
                        "auth": { "enabled": true, "users": { "MARK-user": { "access": "control", "token": { "hash": "MARK-hash", "created": 1 } } } } },
            "log": { "dir": "/MARK-logs" },
            "storage": { "backend": "postgres", "url": "postgres://trp:MARK-password@db/trunk" },
            "monitor": { "checkHosts": ["example.com"] },
            "diagnostics": { "captureDir": "/MARK-iq" },
        }))
        .unwrap();
        let marks = serde_json::to_string(&full).unwrap().matches("MARK-").count();
        assert_eq!(marks, 26, "the config kept what the test marked: {marks}");
        let sent = serde_json::to_value(listen_config(&full)).unwrap();
        assert!(!sent.to_string().contains("MARK-"), "{sent}");
        for gone in ["plugins", "keys", "pluginDelays"] {
            assert!(sent.get(gone).is_none_or(|v| v == &json!({}) || v == &json!([])), "{gone}");
            assert!(sent["systems"][0].get(gone).is_none_or(|v| v == &json!({}) || v == &json!([]) || v == ""), "a system's {gone}");
        }
        assert!(sent["server"].get("auth").is_none(), "the users");
        assert_eq!((&sent["storage"]["backend"], &sent["server"]["port"], &sent["server"]["hostname"]), (&json!("postgres"), &json!(8080), &json!("radio.example")));
        assert_eq!((&sent["systems"][0]["shortName"], &sent["systems"][0]["sites"][0]["controlChannelsHz"][0]), (&json!("x"), &json!(855e6)));
        assert_eq!((&sent["conventional"][0]["channels"][0]["name"], &sent["sources"][0]["serial"]), (&json!("Fire"), &json!("201")));
        assert_eq!((&sent["recording"]["ramSpool"]["enabled"], &sent["monitor"]["checkHosts"][0]), (&json!(true), &json!("example.com")));
    }
}
