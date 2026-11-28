//! What plugins make of calls (`call.product`: a transcript, decoded pages), filed with the call
//! each is of.
//!
//! A product names its call by transmission id (a plugin handed the concluded call) or, from a
//! decoder working on a tap while the call still runs, by the call's id. The second kind waits
//! here until the call concludes and has a transmission id (or is dropped, and takes them with
//! it). Kept, a product goes to the store ([`Change::Product`]), to the call's
//! `<call>.products.jsonl` when its JSON is kept, into the call's entry in the live list, to
//! the interfaces and to the plugins subscribed to its kind.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use trunk_app::protocol::ToClient;
use trunk_core::trunk::SystemId;
use trunk_recorder_plugin::{CallRecord, Product, ProductCall};

use crate::db::Change;
use crate::plugins::delay::About;
use crate::runtime::Ctx;
use crate::runtime::publish;

/// How long the products of a running call wait for it to conclude: longer than any call runs.
const LIVE_FOR: Duration = Duration::from_secs(30 * 60);
/// Products of one running call kept waiting: past this it's a plugin gone wrong.
const LIVE_MOST: usize = 4096;
/// Concluded calls remembered: a product for one of them needs no more than its id.
const DONE_KEPT: usize = 2048;

/// The file beside a call its products are added to, a line each.
pub const SIDECAR: &str = "products.jsonl";

#[derive(Default)]
pub struct Products {
    state: Mutex<State>,
    /// A product came for a running call ([`Products::wait`]).
    came: Condvar,
}

#[derive(Default)]
struct State {
    /// By call id: what was made of calls still running.
    live: HashMap<u64, Waiting>,
    /// The calls concluded last, newest at the back.
    done: VecDeque<Arc<Done>>,
}

struct Waiting {
    since: Instant,
    products: Vec<Product>,
}

/// A concluded call, as much as filing a product with it takes.
struct Done {
    call_num: u64,
    transmission_id: String,
    call: ProductCall,
    /// Whose it is, for the plugins' rules (a system off, a delay, `shareDecrypted`).
    about: About,
    /// Its JSON is kept: its products go beside it.
    sidecar: bool,
}

impl Done {
    fn of(system: SystemId, rel: &str, r: &CallRecord, decrypted: bool, sidecar: bool) -> Done {
        let call = ProductCall { system_id: system.0, short_name: r.short_name.clone(), talkgroup: r.talkgroup, talkgroup_tag: r.talkgroup_tag.clone(), freq_hz: r.freq as f64, path: rel.replace('\\', "/") };
        Done { call_num: r.call_num, transmission_id: r.transmission_id.clone(), call, about: About { decrypted, ..crate::plugins::about_record(system, r) }, sidecar }
    }
}

impl Products {
    /// A plugin's `call.product` (`label`: the plugin's, or its copy's): kept with its call, held
    /// until its call concludes, or left out when its call isn't one we have.
    pub fn take(&self, ctx: &Ctx, label: &str, mut p: Product) {
        if p.kind().is_empty() {
            return;
        }
        p.by = crate::plugins::parse_label(label).0.to_string();
        if p.time <= 0.0 {
            p.time = crate::clock::unix_s();
        }
        if !p.transmission_id.is_empty() {
            match self.filed(ctx, &p) {
                Some(d) => keep(ctx, &d, p),
                None => log::debug!("{label}: a {} for call {}, which isn't one of the last {DONE_KEPT} (Product::of_call says where an older one is)", p.kind(), p.transmission_id),
            }
            return;
        }
        let Some(id) = p.call_id.map(u64::from) else {
            log::debug!("{label}: a {} that names no call", p.kind());
            return;
        };
        let mut st = self.state.lock().unwrap();
        // (Already concluded: a decoder's last pieces come after the call's end.)
        if let Some(d) = st.done.iter().rev().find(|d| d.call_num == id).cloned() {
            drop(st);
            keep(ctx, &d, p);
            return;
        }
        st.live.retain(|_, w| w.since.elapsed() < LIVE_FOR);
        let w = st.live.entry(id).or_insert_with(|| Waiting { since: Instant::now(), products: Vec::new() });
        if w.products.len() < LIVE_MOST {
            w.products.push(p);
        }
        self.came.notify_all();
    }

    /// The concluded call `p` names by transmission id: one of the last, or the one filed where
    /// `p` says ([`Product::of_call`]), looked up in the store.
    fn filed(&self, ctx: &Ctx, p: &Product) -> Option<Arc<Done>> {
        if let Some(d) = self.state.lock().unwrap().done.iter().rev().find(|d| d.transmission_id == p.transmission_id) {
            return Some(d.clone());
        }
        let path = p.call.as_ref().map(|c| c.path.as_str()).filter(|p| !p.is_empty())?;
        let c = ctx.db.one(path).ok().flatten().filter(|c| c.record.transmission_id == p.transmission_id)?;
        Some(Arc::new(Done::of(SystemId(c.record.system_id), &c.path, &c.record, c.record.decrypted, c.json)))
    }

    /// Whether anything was made of running call `call_num`, waiting up to `most` for it (a
    /// data-only call is kept only then).
    pub fn wait(&self, call_num: u64, most: Duration) -> bool {
        let st = self.state.lock().unwrap();
        let (st, _) = self.came.wait_timeout_while(st, most, |st| !st.live.contains_key(&call_num)).unwrap();
        st.live.contains_key(&call_num)
    }

    /// Call `record` of `system` concluded and was filed under `rel` (`sidecar`: its JSON is
    /// kept): what was made of it while it ran is kept now, and what comes later finds it.
    pub fn concluded(&self, ctx: &Ctx, system: SystemId, rel: &str, record: &CallRecord, decrypted: bool, sidecar: bool) {
        if record.transmission_id.is_empty() {
            self.dropped(record.call_num);
            return;
        }
        let d = Arc::new(Done::of(system, rel, record, decrypted, sidecar));
        let waiting = {
            let mut st = self.state.lock().unwrap();
            st.done.push_back(d.clone());
            if st.done.len() > DONE_KEPT {
                st.done.pop_front();
            }
            st.live.remove(&d.call_num)
        };
        for p in waiting.map(|w| w.products).unwrap_or_default() {
            keep(ctx, &d, p);
        }
    }

    /// Call `call_num` wasn't saved: what was made of it goes with it.
    pub fn dropped(&self, call_num: u64) {
        self.state.lock().unwrap().live.remove(&call_num);
    }
}

/// File `p` with its call `d`, and tell everyone.
fn keep(ctx: &Ctx, d: &Done, mut p: Product) {
    (p.transmission_id, p.call_id, p.call) = (d.transmission_id.clone(), None, None);
    ctx.db.send(Change::Product(Box::new(p.clone())));
    if d.sidecar {
        let file = ctx.folders.recordings.path().join(format!("{}.{SIDECAR}", d.call.path));
        let line = serde_json::to_string(&p).unwrap_or_default() + "\n";
        // (The folder says why when it can't be written, as for the call's own files.)
        let _ = ctx.folders.recordings.prepare(&file).and_then(|_| trunk_app::files::append(&file)).and_then(|mut f| f.write_all(line.as_bytes()).map_err(|e| trunk_app::files::Problem::of(trunk_app::files::Doing::Write, &file, &e)));
    }
    p.call = Some(d.call.clone());
    for c in ctx.recent.lock().unwrap().iter_mut().filter(|c| c.record.transmission_id == p.transmission_id) {
        c.products.push(Product { call: None, ..p.clone() });
    }
    publish(&ctx.hub, ToClient::Product(p.clone()));
    if let Some(h) = ctx.plugins.host() {
        h.product(&d.about, &d.call.path, &p);
    }
}
