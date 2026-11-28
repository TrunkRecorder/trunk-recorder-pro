//! Mobility management (EN 300 392-2 clause 16): registrations, and the groups radios join.

use super::mac::Reader;

/// A class of usage (clause 16.10.6) says how a radio uses a group; the standard leaves which means
/// what to the network. Off the air this is the one a radio has selected, as P25's affiliation is;
/// groups it only scans have others.
const SELECTED: u8 = 4;

/// A mobility management PDU.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mobility {
    pub kind: u8,
    /// The groups it says the radio is attached to.
    pub groups: Vec<Attached>,
}

/// A group a radio is attached to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attached {
    pub group: u32,
    /// 0–7.
    pub class: u8,
}

impl Mobility {
    /// D-LOCATION UPDATE ACCEPT.
    const ACCEPT: u8 = 5;
    /// D-ATTACH/DETACH GROUP IDENTITY ACKNOWLEDGEMENT.
    const GROUPS_ACK: u8 = 11;

    /// The radio it's to is registered here.
    pub fn registered(&self) -> bool {
        self.kind == Self::ACCEPT
    }

    /// The group the radio has selected.
    pub fn selected(&self) -> Option<u32> {
        self.groups.iter().find(|g| g.class == SELECTED).map(|g| g.group)
    }

    /// From its PDU type on. One whose groups can't be read through has none.
    pub fn parse(mut r: Reader) -> Option<Mobility> {
        let kind = r.read(4)? as u8;
        let groups = match kind {
            Self::ACCEPT => accept_groups(r),
            Self::GROUPS_ACK => ack_groups(r),
            _ => None,
        };
        Some(Mobility { kind, groups: groups.unwrap_or_default() })
    }
}

/// Type 3 and 4 elements: each one's identifier and contents, after the optional bit.
fn elements<'a>(r: &mut Reader<'a>) -> Option<Vec<(u32, &'a [u8])>> {
    let mut all = Vec::new();
    while r.flag()? {
        let id = r.read(4)?;
        let n = r.read(11)? as usize;
        let body = r.rest().get(..n)?;
        r.skip(n)?;
        all.push((id, body));
    }
    Some(all)
}

/// The group identity downlink elements (identifier 7) among them.
fn groups(elements: Vec<(u32, &[u8])>) -> Option<Vec<Attached>> {
    let mut all = Vec::new();
    for (_, body) in elements.into_iter().filter(|e| e.0 == 7) {
        let mut r = Reader::new(body);
        for _ in 0..r.read(6)? {
            // Detached: why. Attached: its lifetime, then its class.
            let class = if r.flag()? { r.skip(2).map(|_| None)? } else { r.skip(2).and(Some(r.read(3)? as u8)) };
            let address = r.read(2)?;
            let group = r.read(24)?;
            r.skip([0, 24, 0, 48][address as usize])?;
            // (Type 2's is a visitor's number for a group, not the group's own.)
            all.extend(class.filter(|_| address != 2).map(|class| Attached { group, class }));
        }
    }
    Some(all)
}

fn ack_groups(mut r: Reader) -> Option<Vec<Attached>> {
    r.skip(2)?; // accepted or (some) refused, reserved; a refused group is told as detached
    if !r.flag()? {
        return Some(vec![]);
    }
    groups(elements(&mut r)?)
}

fn accept_groups(mut r: Reader) -> Option<Vec<Attached>> {
    r.skip(3)?; // kind of update
    if !r.flag()? {
        return Some(vec![]);
    }
    // SSI, address extension, subscriber class, energy saving, SCCH.
    for n in [24, 24, 16, 14, 6] {
        if r.flag()? {
            r.skip(n)?;
        }
    }
    // Group identity location accept (5): as the acknowledgement, from its accept bit.
    let Some((_, body)) = elements(&mut r)?.into_iter().find(|e| e.0 == 5) else { return Some(vec![]) };
    ack_groups(Reader::new(body))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub fn pack(fields: &[(u32, usize)]) -> Vec<u8> {
        fields.iter().flat_map(|&(v, n)| (0..n).rev().map(move |k| (v >> k & 1) as u8)).collect()
    }

    /// A group identity downlink element of `(class, or None: detached; group)`, and what ends the elements.
    pub fn downlink(groups: &[(Option<u8>, u32)]) -> Vec<u8> {
        let mut body = pack(&[(groups.len() as u32, 6)]);
        for &(class, group) in groups {
            body.extend(match class {
                Some(c) => pack(&[(0, 1), (3, 2), (c as u32, 3), (0, 2), (group, 24)]),
                None => pack(&[(1, 1), (0, 2), (0, 2), (group, 24)]),
            });
        }
        let mut e = pack(&[(1, 1), (7, 4), (body.len() as u32, 11)]);
        e.extend(body);
        e.push(0);
        e
    }

    /// D-ATTACH/DETACH GROUP IDENTITY ACKNOWLEDGEMENT, from its PDU type.
    pub fn ack(refused: bool, groups: &[(Option<u8>, u32)]) -> Vec<u8> {
        let mut p = pack(&[(11, 4), (refused as u32, 1), (0, 1), (1, 1)]);
        p.extend(downlink(groups));
        p
    }

    /// D-LOCATION UPDATE ACCEPT with a subscriber class and SCCH, laid out as a live site's.
    pub fn accept(groups: &[(Option<u8>, u32)]) -> Vec<u8> {
        let mut inner = pack(&[(0, 1), (0, 1), (1, 1)]);
        inner.extend(downlink(groups));
        let mut p = pack(&[(5, 4), (0, 3), (1, 1), (0, 1), (0, 1), (1, 1), (0xffff, 16), (0, 1), (1, 1), (9, 6), (1, 1), (5, 4), (inner.len() as u32, 11)]);
        p.extend(inner);
        p.push(0);
        p
    }

    fn read(bits: &[u8]) -> Mobility {
        Mobility::parse(Reader::new(bits)).unwrap()
    }

    #[test]
    fn a_join_names_its_group() {
        let m = read(&ack(false, &[(Some(4), 2001)]));
        assert_eq!((m.kind, m.registered(), m.selected()), (11, false, Some(2001)));
        // Groups only scanned are none selected; the selected one counts among them.
        assert_eq!(read(&ack(false, &[(Some(3), 2002), (Some(3), 2003)])).selected(), None);
        assert_eq!(read(&ack(false, &[(Some(3), 2002), (Some(4), 2001)])).selected(), Some(2001));
        // Refused, or left: told as detached.
        assert!(read(&ack(true, &[(None, 2001)])).groups.is_empty());
        assert_eq!(read(&ack(true, &[(None, 2001), (Some(4), 2004)])).selected(), Some(2004));
    }

    #[test]
    fn a_registration_names_its_group() {
        let m = read(&accept(&[(Some(4), 2001)]));
        assert_eq!((m.registered(), m.selected()), (true, Some(2001)));
        // Without one, it's a registration all the same.
        let m = read(&pack(&[(5, 4), (0, 3), (0, 1)]));
        assert_eq!((m.registered(), m.groups.len()), (true, 0));
    }

    /// Addresses longer than a group's number are read past; a visitor's isn't the group's own.
    #[test]
    fn longer_addresses() {
        for (kind, extra, want) in [(1, 24, Some(2001)), (2, 0, None), (3, 48, Some(2001))] {
            let mut body = pack(&[(2, 6), (0, 1), (3, 2), (4, 3), (kind, 2), (2001, 24)]);
            body.extend(vec![1; extra]);
            body.extend(pack(&[(0, 1), (3, 2), (3, 3), (0, 2), (2009, 24)]));
            let mut p = pack(&[(11, 4), (0, 2), (1, 1), (1, 1), (7, 4), (body.len() as u32, 11)]);
            p.extend(body);
            p.push(0);
            let m = read(&p);
            assert_eq!((m.selected(), m.groups.last().unwrap().group), (want, 2009), "{kind}");
        }
    }

    /// Cut short anywhere, or any bits at all: no groups made up, no panic.
    #[test]
    fn cut_short() {
        for p in [ack(false, &[(Some(4), 2001), (Some(3), 2002)]), accept(&[(Some(4), 2001)])] {
            for n in 4..p.len() {
                assert!(read(&p[..n]).groups.is_empty(), "{n}");
            }
        }
        let mut x = 1u32;
        for _ in 0..2000 {
            let bits: Vec<u8> = (0..150).map(|_| { x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223); (x >> 24 & 1) as u8 }).collect();
            for kind in [5, 11] {
                let mut p = pack(&[(kind, 4)]);
                p.extend(&bits);
                read(&p);
            }
        }
    }
}
