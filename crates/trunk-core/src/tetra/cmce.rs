//! What a TM-SDU carries: the LLC's basic link (EN 300 392-2 clause 21.2), the MLE's protocol
//! discriminator (table 18.87) and circuit mode control (CMCE, clause 14): calls.

use super::mac::Reader;
use super::mle::Broadcast;
use super::mm::Mobility;

/// The protocol a TL-SDU is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pdu {
    /// Mobility management: registrations, and the groups radios join.
    Mobility(Mobility),
    Call(Call),
    /// D-STATUS: the radio it's from (None: not said), and its number.
    Status { from: Option<u32>, status: u16 },
    /// D-SDS-DATA: the radio it's from, and what it is. Its contents aren't read.
    ShortData { from: Option<u32>, data: ShortData },
    /// Packet data.
    PacketData,
    /// The MLE's D-NWRK-BROADCAST.
    Broadcast(Broadcast),
    /// The MLE's others: the PDU type.
    Mle(u8),
    /// Anything else: the protocol discriminator.
    Other(u8),
}

/// What a short data message is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortData {
    /// User defined: its bits.
    User(u16),
    /// Type 4: its protocol identifier (clause 29.4.3.9), and its bits in all.
    Protocol(u8, u16),
}

impl std::fmt::Display for ShortData {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let (what, bits) = match *self {
            ShortData::User(bits) => ("user defined".into(), bits),
            ShortData::Protocol(id, bits) => {
                let name = match id {
                    0x02 | 0x82 => "text",
                    0x09 | 0x89 => "immediate text",
                    0x03 | 0x83 | 0x0a => "location",
                    0x04 | 0x84 => "WAP",
                    0x0c | 0x8c => "part of a longer one",
                    _ => "",
                };
                (if name.is_empty() { format!("protocol 0x{id:02x}") } else { name.into() }, bits)
            }
        };
        write!(f, "{what}, {bits} bits")
    }
}

/// Why a call ended (its disconnect cause).
pub fn cause(c: u8) -> String {
    const CAUSES: [&str; 16] = [
        "cause not said", "by the user", "called party busy", "called party not reachable", "called party has no encryption", "network congestion", "not allowed",
        "incompatible traffic", "service not available", "pre-empted", "invalid call identifier", "call rejected", "no idle call control", "timer expired",
        "by the network", "acknowledgement not completed",
    ];
    CAUSES.get(c as usize).map_or(format!("cause {c}"), |s| s.to_string())
}

/// A call's CMCE PDU.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub kind: CallKind,
    /// Call identifier (none on facility).
    pub id: Option<u16>,
    /// The calling or transmitting radio.
    pub radio: Option<u32>,
    pub service: Option<Service>,
    pub priority: Option<u8>,
    /// D-TX GRANTED: the traffic is end-to-end encrypted.
    pub encrypted: Option<bool>,
    /// D-RELEASE, D-DISCONNECT: [`cause`].
    pub cause: Option<u8>,
    /// D-CONNECT: what only a call between two radios has, hook signalling (it was answered) or duplex.
    pub one_to_one: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallKind {
    Alert,
    Proceeding,
    Connect,
    ConnectAck,
    Disconnect,
    Info,
    Release,
    Setup,
    Status,
    TxCeased,
    TxContinue,
    TxGranted,
    TxWait,
    TxInterrupt,
    Restore,
    ShortData,
    Facility,
}

/// Basic service information (clause 14.8.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Service {
    /// Circuit mode: 0 speech (TCH/S), the rest data.
    pub circuit: u8,
    /// End-to-end encrypted.
    pub encrypted: bool,
    /// 0 point to point, 1 to many, 2 acknowledged, 3 broadcast.
    pub communication: u8,
}

impl Service {
    fn of(v: u32) -> Service {
        Service { circuit: (v >> 5) as u8, encrypted: v >> 4 & 1 != 0, communication: (v >> 2 & 3) as u8 }
    }
    pub fn speech(&self) -> bool {
        self.circuit == 0
    }
    pub fn group(&self) -> bool {
        self.communication != 0
    }
}

/// The LLC frame check (annex C): CRC-32 over the TL-SDU, preset to ones and inverted.
fn fcs(bits: &[u8]) -> u32 {
    let mut r = u32::MAX;
    for &b in bits {
        let fb = b as u32 ^ r >> 31;
        r = r << 1 ^ if fb != 0 { 0x04c1_1db7 } else { 0 };
    }
    !r
}

/// The PDU in a whole, clear TM-SDU; None for the advanced link, an acknowledgement alone, or a bad frame check.
pub fn parse(sdu: &[u8]) -> Option<Pdu> {
    let mut r = Reader::new(sdu);
    let kind = r.read(4)?;
    if kind >= 8 {
        return None;
    }
    // BL-ADATA, BL-DATA, BL-UDATA, BL-ACK: their sequence numbers; then the same with a frame check.
    r.skip([2, 1, 0, 1][kind as usize & 3])?;
    let mut body = r.rest();
    if kind >= 4 {
        let (data, check) = body.split_at(body.len().checked_sub(32)?);
        if Reader::new(check).read(32)? != fcs(data) {
            return None;
        }
        body = data;
    }
    let mut r = Reader::new(body);
    Some(match r.read(3)? {
        1 => Pdu::Mobility(Mobility::parse(r)?),
        2 => circuit(r)?,
        4 => Pdu::PacketData,
        5 => match r.read(3)? as u8 {
            2 => Pdu::Broadcast(Broadcast::parse(r)?),
            k => Pdu::Mle(k),
        },
        p => Pdu::Other(p as u8),
    })
}

/// A type-2 element's presence bit, then the element.
fn optional(r: &mut Reader, n: usize) -> Option<Option<u32>> {
    Some(if r.flag()? { Some(r.read(n)?) } else { None })
}

/// The type-2 party element: its type, then an SSI (and for type 2 its extension, read past).
fn party(r: &mut Reader) -> Option<Option<u32>> {
    if !r.flag()? {
        return Some(None);
    }
    match r.read(2)? {
        1 => Some(Some(r.read(24)?)),
        2 => {
            let ssi = r.read(24)?;
            r.skip(24)?;
            Some(Some(ssi))
        }
        _ => Some(None),
    }
}

/// D-STATUS's and D-SDS-DATA's calling party: its type, then an SSI (and for type 2 its extension, read past).
fn calling(r: &mut Reader) -> Option<Option<u32>> {
    let kind = r.read(2)?;
    if !matches!(kind, 1 | 2) {
        return Some(None);
    }
    let ssi = r.read(24)?;
    r.skip(if kind == 2 { 24 } else { 0 })?;
    Some(Some(ssi))
}

fn circuit(mut r: Reader) -> Option<Pdu> {
    Some(match r.clone().read(5)? {
        8 => {
            r.skip(5)?;
            Pdu::Status { from: calling(&mut r)?, status: r.read(16)? as u16 }
        }
        15 => {
            r.skip(5)?;
            let from = calling(&mut r)?;
            let data = match r.read(2)? {
                3 => {
                    let bits = r.read(11)? as u16;
                    ShortData::Protocol(r.read(8)? as u8, bits)
                }
                k => ShortData::User(16 << k),
            };
            Pdu::ShortData { from, data }
        }
        _ => Pdu::Call(call(r)?),
    })
}

fn call(mut r: Reader) -> Option<Call> {
    use CallKind::*;
    const KINDS: [CallKind; 17] =
        [Alert, Proceeding, Connect, ConnectAck, Disconnect, Info, Release, Setup, Status, TxCeased, TxContinue, TxGranted, TxWait, TxInterrupt, Restore, ShortData, Facility];
    let kind = *KINDS.get(r.read(5)? as usize)?;
    let mut c = Call { kind, id: None, radio: None, service: None, priority: None, encrypted: None, cause: None, one_to_one: false };
    if kind == Facility {
        return Some(c);
    }
    c.id = Some(r.read(14)? as u16);
    match kind {
        Setup => {
            r.skip(4 + 1 + 1)?; // time-out, hook, simplex / duplex
            c.service = Some(Service::of(r.read(8)?));
            r.skip(2 + 1)?; // grant, request permission
            c.priority = Some(r.read(4)? as u8);
            if r.flag()? {
                optional(&mut r, 6)?; // notification
                optional(&mut r, 24)?; // temporary address
                c.radio = party(&mut r)?;
            }
        }
        Connect => {
            r.skip(4)?; // time-out
            c.one_to_one = r.read(2)? != 0; // hook, simplex / duplex
            r.skip(2 + 1 + 1)?; // grant, permission, ownership
            if r.flag()? {
                c.priority = optional(&mut r, 4)?.map(|p| p as u8);
                c.service = optional(&mut r, 8)?.map(Service::of);
            }
        }
        Release | Disconnect => c.cause = r.read(5).map(|c| c as u8),
        TxGranted | TxInterrupt => {
            r.skip(2 + 1)?; // grant, request permission
            // Encryption control: 0 clear, 1 end-to-end encrypted.
            c.encrypted = Some(r.flag()?);
            r.skip(1)?;
            if r.flag()? {
                optional(&mut r, 6)?; // notification
                c.radio = party(&mut r)?;
            }
        }
        _ => {}
    }
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(fields: &[(u32, usize)]) -> Vec<u8> {
        fields.iter().flat_map(|&(v, n)| (0..n).rev().map(move |k| (v >> k & 1) as u8)).collect()
    }

    /// D-SETUP: a clear group speech call from radio 4210, priority 3, in BL-UDATA.
    const SETUP: [(u32, usize); 16] = [(2, 4), (2, 3), (7, 5), (291, 14), (5, 4), (0, 2), (0b0000_0100, 8), (3, 2), (0, 1), (3, 4), (1, 1), (0, 1), (0, 1), (1, 1), (1, 2), (4210, 24)];

    #[test]
    fn setup_names_the_call() {
        let Some(Pdu::Call(c)) = parse(&pack(&SETUP)) else { panic!() };
        assert_eq!((c.kind, c.id, c.radio, c.priority), (CallKind::Setup, Some(291), Some(4210), Some(3)));
        let s = c.service.unwrap();
        assert!(s.speech() && s.group() && !s.encrypted);
        // Cut short: nothing.
        for n in 0..pack(&SETUP).len() {
            assert!(parse(&pack(&SETUP)[..n]).is_none(), "{n}");
        }
    }

    /// The layout of dsd-neo's D-TX GRANTED (test_cmce_pdus.c).
    #[test]
    fn granted_names_the_talker() {
        let p = pack(&[(2, 4), (2, 3), (11, 5), (1, 14), (3, 2), (1, 1), (0, 1), (0, 1), (1, 1), (0, 1), (1, 1), (1, 2), (4211, 24), (0, 1)]);
        let Some(Pdu::Call(c)) = parse(&p) else { panic!() };
        assert_eq!((c.kind, c.id, c.radio, c.encrypted), (CallKind::TxGranted, Some(1), Some(4211), Some(false)));
    }

    /// D-CONNECT: a call answered, or duplex, is one between two radios.
    #[test]
    fn connect_says_whats_between_two() {
        for (hook, duplex) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let p = pack(&[(2, 4), (2, 3), (2, 5), (310, 14), (5, 4), (hook, 1), (duplex, 1), (0, 2), (0, 1), (0, 1), (1, 1), (0, 1), (1, 1), (0b0000_0100, 8), (0, 1)]);
            let Some(Pdu::Call(c)) = parse(&p) else { panic!() };
            assert_eq!((c.kind, c.id, c.one_to_one, c.service.map(|s| s.group())), (CallKind::Connect, Some(310), hook + duplex != 0, Some(true)));
        }
    }

    /// D-STATUS, D-SDS-DATA of each type, D-RELEASE: who from, what, why.
    #[test]
    fn messages_and_endings() {
        for (party, from) in [(vec![(1, 2), (4210, 24)], Some(4210)), (vec![(2, 2), (4210, 24), (99, 24)], Some(4210)), (vec![(0, 2)], None)] {
            let of = |kind: u32, rest: &[(u32, usize)]| parse(&pack(&[&[(2, 4), (2, 3), (kind, 5)], &party[..], rest].concat()));
            assert_eq!(of(8, &[(52_002, 16)]), Some(Pdu::Status { from, status: 52_002 }));
            assert_eq!(of(8, &[(1, 15)]), None, "cut short");
            for (k, bits) in [(0, 16), (1, 32), (2, 64)] {
                assert_eq!(of(15, &[(k, 2)]), Some(Pdu::ShortData { from, data: ShortData::User(bits) }));
            }
            assert_eq!(of(15, &[(3, 2), (88, 11), (2, 8)]), Some(Pdu::ShortData { from, data: ShortData::Protocol(2, 88) }));
            assert_eq!(of(15, &[(3, 2), (88, 11)]), None, "cut short");
        }
        assert_eq!([ShortData::User(32), ShortData::Protocol(2, 88), ShortData::Protocol(0x55, 8)].map(|d| d.to_string()), ["user defined, 32 bits", "text, 88 bits", "protocol 0x55, 8 bits"]);
        for kind in [4, 6] {
            let Some(Pdu::Call(c)) = parse(&pack(&[(2, 4), (2, 3), (kind, 5), (301, 14), (13, 5)])) else { panic!() };
            assert_eq!((c.id, c.cause), (Some(301), Some(13)));
            // Ended all the same without it.
            let Some(Pdu::Call(c)) = parse(&pack(&[(2, 4), (2, 3), (kind, 5), (301, 14)])) else { panic!() };
            assert_eq!((c.id, c.cause), (Some(301), None));
        }
        assert_eq!((cause(13).as_str(), cause(30).as_str()), ("timer expired", "cause 30"));
    }

    #[test]
    fn a_frame_check_is_checked() {
        let body = pack(&SETUP[1..]);
        let mut p = pack(&[(6, 4)]);
        p.extend(&body);
        p.extend(pack(&[(fcs(&body), 32)]));
        assert!(matches!(parse(&p), Some(Pdu::Call(_))));
        p[20] ^= 1;
        assert_eq!(parse(&p), None);
    }
}
