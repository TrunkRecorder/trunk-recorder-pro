//! What a site says about itself, and the site lock that holds a system to
//! one site ([`super::engine::SiteConfig::expect`]).
//!
//! An identity is a set of facts, each an [`IdField`]. Each protocol says
//! which it states ([`super::control::ControlChannel::identity_fields`]):
//! P25 its NAC, WACN, System ID, RFSS and site; SmartNet its System ID and
//! site; DMR its colour code (Tier III also its network and site); NXDN its
//! RAN (Type-C also its system and site). The lock compares only the fields the
//! protocol states, so a lock field it never sends can't hold a system off
//! for ever, and a new protocol adds the fields it has instead of more
//! optional ones.

use std::collections::BTreeMap;

// (The plugin SDK's: the shape the config, status and plugins share.)
pub use trunk_recorder_plugin::SiteIdentity;

/// One fact a control channel can state about its site.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IdField {
    /// P25 Network Access Code.
    Nac,
    /// P25 Wide Area Communications Network.
    Wacn,
    /// System ID (P25, SmartNet).
    SysId,
    /// P25 RF subsystem.
    Rfss,
    /// Site (P25; SmartNet when OBT sends it; NXDN Type-C; DMR Tier III).
    Site,
    /// DMR colour code.
    ColorCode,
    /// DMR Tier III network (its System Identity Code's).
    Network,
    /// NXDN Radio Access Number.
    Ran,
}

impl IdField {
    pub const ALL: [IdField; 8] = [IdField::Nac, IdField::Wacn, IdField::SysId, IdField::Rfss, IdField::Site, IdField::ColorCode, IdField::Network, IdField::Ran];

    /// As log lines name it.
    pub fn label(self) -> &'static str {
        match self {
            IdField::Nac => "NAC",
            IdField::Wacn => "WACN",
            IdField::SysId => "SysID",
            IdField::Rfss => "RFSS",
            IdField::Site => "site",
            IdField::ColorCode => "CC",
            IdField::Network => "network",
            IdField::Ran => "RAN",
        }
    }

    /// As the config and the interface name it.
    pub fn key(self) -> &'static str {
        match self {
            IdField::Nac => "nac",
            IdField::Wacn => "wacn",
            IdField::SysId => "sysId",
            IdField::Rfss => "rfss",
            IdField::Site => "site",
            IdField::ColorCode => "colorCode",
            IdField::Network => "network",
            IdField::Ran => "ran",
        }
    }

    /// A value of it as written: NAC, WACN and System ID in hex (as the air
    /// and RadioReference show them), the others in decimal.
    fn show(self, v: u32) -> String {
        if matches!(self, IdField::Nac | IdField::Wacn | IdField::SysId) { format!("{v:X}") } else { v.to_string() }
    }
}

/// What a site has said about itself (or, as a site lock, what it must say).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Identity(BTreeMap<IdField, u32>);

impl Identity {
    pub fn get(&self, f: IdField) -> Option<u32> {
        self.0.get(&f).copied()
    }
    pub fn set(&mut self, f: IdField, v: u32) {
        self.0.insert(f, v);
    }
    /// Set it, or (None) forget it.
    pub fn set_opt(&mut self, f: IdField, v: Option<u32>) {
        match v {
            Some(v) => self.set(f, v),
            None => {
                self.0.remove(&f);
            }
        }
    }
    /// With `f` set to `v`.
    pub fn with(mut self, f: IdField, v: u32) -> Identity {
        self.set(f, v);
        self
    }
    pub fn iter(&self) -> impl Iterator<Item = (IdField, u32)> + '_ {
        self.0.iter().map(|(&f, &v)| (f, v))
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    // The fields by name, for what is a protocol's own (P25's Phase 2 key and
    // site groups, a preferred NAC).
    pub fn nac(&self) -> Option<u16> {
        self.get(IdField::Nac).map(|v| v as u16)
    }
    pub fn wacn(&self) -> Option<u32> {
        self.get(IdField::Wacn)
    }
    pub fn sys_id(&self) -> Option<u32> {
        self.get(IdField::SysId)
    }
    pub fn rfss(&self) -> Option<u32> {
        self.get(IdField::Rfss)
    }
    pub fn site(&self) -> Option<u32> {
        self.get(IdField::Site)
    }

    /// As the config, status and plugins write it ([`SiteIdentity`]).
    pub fn to_site(&self) -> SiteIdentity {
        let narrow = |f: IdField| self.get(f).map(|v| v as u8);
        SiteIdentity {
            nac: self.nac(),
            wacn: self.wacn(),
            sys_id: self.sys_id(),
            rfss: self.rfss(),
            site: self.site(),
            color_code: narrow(IdField::ColorCode),
            network: self.get(IdField::Network),
            ran: narrow(IdField::Ran),
        }
    }

    /// The fields `s` names.
    pub fn of_site(s: &SiteIdentity) -> Identity {
        let mut id = Identity::default();
        id.set_opt(IdField::Nac, s.nac.map(u32::from));
        id.set_opt(IdField::Wacn, s.wacn);
        id.set_opt(IdField::SysId, s.sys_id);
        id.set_opt(IdField::Rfss, s.rfss);
        id.set_opt(IdField::Site, s.site);
        id.set_opt(IdField::ColorCode, s.color_code.map(u32::from));
        id.set_opt(IdField::Network, s.network);
        id.set_opt(IdField::Ran, s.ran.map(u32::from));
        id
    }

    /// The `fields` both know and disagree on, e.g. "site 3 (expected 4)";
    /// None when they agree.
    pub fn conflict(&self, expect: &Identity, fields: &[IdField]) -> Option<String> {
        let d: Vec<String> = IdField::ALL
            .into_iter()
            .filter(|f| fields.contains(f))
            .filter_map(|f| {
                let (g, w) = (self.get(f)?, expect.get(f)?);
                (g != w).then(|| format!("{} {} (expected {})", f.label(), f.show(g), f.show(w)))
            })
            .collect();
        (!d.is_empty()).then(|| d.join(", "))
    }

    /// Each of `fields` with its value, "?" when not heard: "SysID 50 site 14".
    pub fn describe(&self, fields: &[IdField]) -> String {
        let shown = |f: IdField| self.get(f).map_or("?".into(), |v| f.show(v));
        IdField::ALL.into_iter().filter(|f| fields.contains(f)).map(|f| format!("{} {}", f.label(), shown(f))).collect::<Vec<_>>().join(" ")
    }

    /// Every one of `fields` that `expect` names is known here.
    pub fn confirms(&self, expect: &Identity, fields: &[IdField]) -> bool {
        fields.iter().all(|&f| expect.get(f).is_none() || self.get(f).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_and_confirmation() {
        let all = &IdField::ALL;
        let expect = Identity::default().with(IdField::Nac, 0x443).with(IdField::Site, 4);
        let mut heard = Identity::default().with(IdField::Nac, 0x443);
        assert_eq!(heard.conflict(&expect, all), None);
        assert!(!heard.confirms(&expect, all), "site not heard yet");
        heard.set(IdField::Site, 3);
        heard.set(IdField::Rfss, 1);
        assert_eq!(heard.conflict(&expect, all).as_deref(), Some("site 3 (expected 4)"));
        heard.set(IdField::Site, 4);
        assert!(heard.confirms(&expect, all) && heard.conflict(&expect, all).is_none());
        heard.set(IdField::Nac, 0x1a);
        assert_eq!(heard.conflict(&expect, all).as_deref(), Some("NAC 1A (expected 443)"));
        assert!(Identity::default().confirms(&Identity::default(), all));
    }

    /// A lock field the protocol never states is ignored (a SmartNet system
    /// locked on a NAC used to wait for it for ever).
    #[test]
    fn only_the_protocols_fields_are_compared() {
        let smartnet = &[IdField::SysId, IdField::Site];
        let expect = Identity::default().with(IdField::Nac, 0x443).with(IdField::SysId, 0x2011);
        let heard = Identity::default().with(IdField::SysId, 0x2011);
        assert!(heard.confirms(&expect, smartnet));
        assert_eq!(heard.conflict(&expect, smartnet), None);
        assert!(!heard.confirms(&expect, &IdField::ALL));
        assert_eq!(heard.clone().with(IdField::SysId, 0x2012).conflict(&expect, smartnet).as_deref(), Some("SysID 2012 (expected 2011)"));
    }
}
