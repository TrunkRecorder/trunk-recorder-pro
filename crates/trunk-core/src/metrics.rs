//! Measurement plumbing. The decoder's parts keep plain running counters and
//! current values in their own fields — no locks, no allocation, nothing on
//! the sample path beyond an increment — and say what they are when asked
//! ([`Instrumented::report`]), about once a second. Rates, history, storage
//! and presentation belong to whoever asks (trunk-app's `stats`), not to them.
//!
//! Names are short, `/`-separated and stable: they become series keys
//! (`sys/<shortName>/cc/good`), so renaming one loses its history.

/// Where an [`Instrumented`] part reports to.
pub trait Sink {
    /// A running total (it only grows, but may restart from 0 when the part
    /// is replaced: a control channel retuned). The reader makes rates of it.
    fn counter(&mut self, name: &str, total: u64);
    /// A current value (a level, an estimate). Non-finite values are dropped.
    fn gauge(&mut self, name: &str, value: f64);
}

/// A part that can say what it has measured.
pub trait Instrumented {
    fn report(&self, sink: &mut dyn Sink);
}

/// A sink that puts `prefix/` before every name.
pub struct Scoped<'a> {
    prefix: String,
    inner: &'a mut dyn Sink,
    name: String,
}

impl<'a> Scoped<'a> {
    pub fn new(inner: &'a mut dyn Sink, prefix: impl Into<String>) -> Scoped<'a> {
        Scoped { prefix: prefix.into(), inner, name: String::new() }
    }

    fn key(&mut self, name: &str) -> &str {
        self.name.clear();
        self.name.push_str(&self.prefix);
        self.name.push('/');
        self.name.push_str(name);
        &self.name
    }
}

impl Sink for Scoped<'_> {
    fn counter(&mut self, name: &str, total: u64) {
        let k = self.key(name).to_string();
        self.inner.counter(&k, total);
    }
    fn gauge(&mut self, name: &str, value: f64) {
        if value.is_finite() {
            let k = self.key(name).to_string();
            self.inner.gauge(&k, value);
        }
    }
}

/// A series key's part: no `/` (it separates parts), trimmed.
pub fn key_part(s: &str) -> String {
    s.trim().replace('/', "_")
}

/// |X|² per FFT bin (the channelizer's units) as dBFS, for an `n`-point FFT.
pub fn bin_dbfs(power: f64, n: usize) -> f64 {
    10.0 * (power / (n as f64 * n as f64) + 1e-14).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Collect(Vec<(String, f64)>);
    impl Sink for Collect {
        fn counter(&mut self, name: &str, total: u64) {
            self.0.push((name.to_string(), total as f64));
        }
        fn gauge(&mut self, name: &str, value: f64) {
            self.0.push((name.to_string(), value));
        }
    }

    #[test]
    fn scopes_nest_and_drop_non_finite_gauges() {
        let mut c = Collect::default();
        {
            let mut a = Scoped::new(&mut c, "sys/dcfd");
            let mut b = Scoped::new(&mut a, "cc");
            b.counter("good", 7);
            b.gauge("sep", f64::INFINITY);
            b.gauge("offset", -12.5);
        }
        assert_eq!(c.0, vec![("sys/dcfd/cc/good".to_string(), 7.0), ("sys/dcfd/cc/offset".to_string(), -12.5)]);
        assert_eq!(key_part(" file a/b.cu8 "), "file a_b.cu8");
    }
}
