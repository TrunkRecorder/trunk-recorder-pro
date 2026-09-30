//! Talkgroup patches — Trunk Recorder's System_impl::talkgroup_patches.
//!
//! A patch is a supergroup (the talkgroup the patched radios are granted on)
//! and the talkgroups patched into it. The supergroup is a member too, so a
//! call on any of them sees the whole patch. Each member has its own
//! last-heard time: systems repeat a patch's add message while it stands, and
//! a member not repeated for `hold_s` has left it (Motorola never says). A
//! delete that names members removes those; one that names none (SmartNet's
//! cancel) removes the patch. Time is the sample clock.

use std::collections::{BTreeMap, HashMap};

use super::message::Patch;

/// How long a patch member lasts unrepeated, P25 (Trunk Recorder's 10 s).
pub const P25_HOLD_S: f64 = 10.0;
/// … and SmartNet. WMATA repeats a standing patch every 0.4 s (at most 2 s
/// apart in 5 minutes) and cancels it three times when it ends, so this only
/// matters when all three cancels are lost.
pub const SMARTNET_HOLD_S: f64 = 4.0;

#[derive(Clone, Debug)]
pub struct Patches {
    /// Supergroup → member → last heard.
    groups: HashMap<u32, BTreeMap<u32, f64>>,
    pub hold_s: f64,
}

impl Default for Patches {
    fn default() -> Self {
        Patches { groups: HashMap::new(), hold_s: P25_HOLD_S }
    }
}

impl Patches {
    /// An add (or its repeat): the supergroup and its named members are current. True when one was new.
    pub fn add(&mut self, p: &Patch, time_s: f64) -> bool {
        if p.sg == 0 {
            return false;
        }
        let g = self.groups.entry(p.sg).or_default();
        let mut new = false;
        for tg in std::iter::once(p.sg).chain(p.ga.iter().copied()).filter(|&t| t != 0) {
            new |= g.insert(tg, time_s).is_none();
        }
        new
    }

    /// A delete: its named members leave; none named, the whole patch goes.
    pub fn delete(&mut self, p: &Patch) {
        let named: Vec<u32> = p.ga.iter().copied().filter(|&t| t != 0 && t != p.sg).collect();
        if named.is_empty() {
            self.groups.remove(&p.sg);
            return;
        }
        if let Some(g) = self.groups.get_mut(&p.sg) {
            for tg in named {
                g.remove(&tg);
            }
            // Only the supergroup left: nothing is patched.
            if g.keys().all(|&t| t == p.sg) {
                self.groups.remove(&p.sg);
            }
        }
    }

    /// Drop members not heard for `hold_s`, and patches left with one member or none.
    pub fn expire(&mut self, now_s: f64) {
        let hold = self.hold_s;
        self.groups.retain(|_, g| {
            g.retain(|_, &mut t| now_s - t <= hold);
            g.len() > 1
        });
    }

    /// Every talkgroup patched with `tg`, `tg` included, ascending; empty when it's in no patch.
    pub fn members_of(&self, tg: u32) -> Vec<u32> {
        let mut out: Vec<u32> = self.groups.values().filter(|g| g.contains_key(&tg)).flat_map(|g| g.keys().copied()).collect();
        out.sort_unstable();
        out.dedup();
        if out.len() < 2 {
            out.clear();
        }
        out
    }

    /// The patches standing now: (supergroup, the others patched into it), by supergroup.
    pub fn active(&self) -> Vec<(u32, Vec<u32>)> {
        let mut out: Vec<(u32, Vec<u32>)> = self
            .groups
            .iter()
            .filter(|(_, g)| g.len() > 1)
            .map(|(&sg, g)| (sg, g.keys().copied().filter(|&t| t != sg).collect()))
            .collect();
        out.sort_unstable_by_key(|p| p.0);
        out
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(sg: u32, ga: [u32; 3]) -> Patch {
        Patch { sg, ga }
    }

    #[test]
    fn moto_add_includes_the_supergroup() {
        let mut x = Patches::default();
        assert!(x.add(&p(65001, [101, 202, 0]), 0.0));
        assert!(!x.add(&p(65001, [101, 202, 0]), 1.0));
        assert_eq!(x.members_of(202), vec![101, 202, 65001]);
        assert_eq!(x.members_of(65001), vec![101, 202, 65001]);
        assert!(x.members_of(303).is_empty());
        assert_eq!(x.active(), vec![(65001, vec![101, 202])]);
    }

    #[test]
    fn members_expire_one_by_one() {
        let mut x = Patches::default();
        x.add(&p(65001, [101, 202, 0]), 0.0);
        // 202 drops out of the repeats; 101 stays.
        x.add(&p(65001, [101, 0, 0]), 8.0);
        x.expire(12.0);
        assert_eq!(x.members_of(65001), vec![101, 65001]);
        assert!(x.members_of(202).is_empty());
        x.expire(19.0);
        assert!(x.is_empty());
    }

    #[test]
    fn macom_delete_removes_only_the_named_member() {
        let mut x = Patches::default();
        x.add(&p(500, [101, 101, 101]), 0.0);
        x.add(&p(500, [202, 202, 202]), 0.0);
        x.delete(&p(500, [101, 101, 101]));
        assert_eq!(x.members_of(500), vec![202, 500]);
        x.delete(&p(500, [202, 202, 202]));
        assert!(x.is_empty());
    }

    #[test]
    fn a_cancel_naming_no_members_ends_the_patch() {
        let mut x = Patches::default();
        x.add(&p(4800, [160, 0, 0]), 0.0);
        x.delete(&p(4800, [0; 3]));
        assert!(x.is_empty());
    }

    #[test]
    fn a_talkgroup_in_two_patches_sees_both() {
        let mut x = Patches::default();
        x.add(&p(1, [10, 0, 0]), 0.0);
        x.add(&p(2, [10, 20, 0]), 0.0);
        assert_eq!(x.members_of(10), vec![1, 2, 10, 20]);
        assert_eq!(x.members_of(1), vec![1, 10]);
    }
}
