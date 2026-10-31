//! The one SQL layer both backends share. The store's work (`ops.rs`) is
//! written once against [`Exec`]: statements with `$1, $2…` parameters and
//! dialect-neutral [`V`] values, rows of the same. A backend only connects
//! and runs them (SQLite: `?N` for `$N`; PostgreSQL: values converted to each
//! parameter's own type, which it says when a statement is prepared).

/// A value in or out of the database, the same in either dialect.
#[derive(Clone, Debug, PartialEq)]
pub enum V {
    Null,
    Int(i64),
    Real(f64),
    Bool(bool),
    Text(String),
}

impl From<i64> for V {
    fn from(v: i64) -> V {
        V::Int(v)
    }
}
impl From<i32> for V {
    fn from(v: i32) -> V {
        V::Int(v.into())
    }
}
impl From<u32> for V {
    fn from(v: u32) -> V {
        V::Int(v.into())
    }
}
impl From<usize> for V {
    fn from(v: usize) -> V {
        V::Int(v as i64)
    }
}
impl From<f64> for V {
    fn from(v: f64) -> V {
        V::Real(v)
    }
}
impl From<f32> for V {
    fn from(v: f32) -> V {
        V::Real(v.into())
    }
}
impl From<bool> for V {
    fn from(v: bool) -> V {
        V::Bool(v)
    }
}
impl From<&str> for V {
    fn from(v: &str) -> V {
        V::Text(v.to_string())
    }
}
impl From<String> for V {
    fn from(v: String) -> V {
        V::Text(v)
    }
}
impl From<&String> for V {
    fn from(v: &String) -> V {
        V::Text(v.clone())
    }
}
impl<T: Into<V>> From<Option<T>> for V {
    fn from(v: Option<T>) -> V {
        v.map_or(V::Null, Into::into)
    }
}

/// Values for a statement's `$1, $2…`.
#[macro_export]
macro_rules! vals {
    ($($v:expr),* $(,)?) => { &[$($crate::db::sql::V::from($v)),*] };
}

/// A row out of the database.
#[derive(Clone, Debug)]
pub struct Row(pub Vec<V>);

impl Row {
    fn at(&self, i: usize) -> &V {
        self.0.get(i).unwrap_or(&V::Null)
    }

    pub fn opt_int(&self, i: usize) -> Option<i64> {
        match self.at(i) {
            V::Int(v) => Some(*v),
            V::Real(v) => Some(*v as i64),
            V::Bool(b) => Some(i64::from(*b)),
            _ => None,
        }
    }

    pub fn int(&self, i: usize) -> i64 {
        self.opt_int(i).unwrap_or(0)
    }

    pub fn opt_real(&self, i: usize) -> Option<f64> {
        match self.at(i) {
            V::Real(v) => Some(*v),
            V::Int(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn real(&self, i: usize) -> f64 {
        self.opt_real(i).unwrap_or(0.0)
    }

    /// A boolean (SQLite keeps them as 0 and 1).
    pub fn bool(&self, i: usize) -> bool {
        match self.at(i) {
            V::Bool(b) => *b,
            V::Int(v) => *v != 0,
            _ => false,
        }
    }

    pub fn opt_text(&self, i: usize) -> Option<String> {
        match self.at(i) {
            V::Text(s) => Some(s.clone()),
            _ => None,
        }
    }

    pub fn text(&self, i: usize) -> String {
        self.opt_text(i).unwrap_or_default()
    }
}

/// Which database a statement runs on: for the few things that differ (the
/// schema's types, a trigger in tests).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Postgres,
}

/// Running statements: what the store's work needs of a backend. Errors are
/// strings (logged, or said to whoever asked).
pub trait Exec {
    /// (For what differs by dialect beyond the schema: the catalog's sizes, a test's trigger.)
    fn dialect(&self) -> Dialect;
    /// Run `sql` with `ps` for its `$1, $2…`; the rows it changed.
    fn execute(&mut self, sql: &str, ps: &[V]) -> Result<u64, String>;
    /// Run `sql` with `ps`; the rows it returns.
    fn query(&mut self, sql: &str, ps: &[V]) -> Result<Vec<Row>, String>;
    /// Run statements without parameters (DDL, savepoints).
    fn batch(&mut self, sql: &str) -> Result<(), String>;

    /// The first row, if any.
    fn query_opt(&mut self, sql: &str, ps: &[V]) -> Result<Option<Row>, String> {
        Ok(self.query(sql, ps)?.into_iter().next())
    }
}

/// `f` in a savepoint: all of what it did, or (an error) none of it. In
/// PostgreSQL this is also what keeps a failed statement from aborting the
/// whole transaction (whose commit would then quietly roll back).
pub fn savepoint(x: &mut dyn Exec, f: impl FnOnce(&mut dyn Exec) -> Result<(), String>) -> Result<(), String> {
    x.batch("savepoint change")?;
    match f(x) {
        Ok(()) => x.batch("release savepoint change"),
        Err(e) => {
            let _ = x.batch("rollback to savepoint change");
            let _ = x.batch("release savepoint change");
            Err(e)
        }
    }
}

/// `sql` with each `$N` as SQLite's `?N` (the same numbered parameter).
pub fn question_marks(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek().is_some_and(char::is_ascii_digit) {
            out.push('?');
        } else {
            out.push(c);
        }
    }
    out
}

/// Builds a statement's parameters as it's written: [`Params::add`] says
/// the next `$N` for a value.
#[derive(Default)]
pub struct Params(pub Vec<V>);

impl Params {
    pub fn add(&mut self, v: impl Into<V>) -> String {
        self.0.push(v.into());
        format!("${}", self.0.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_parameters_for_sqlite() {
        assert_eq!(question_marks("update calls set audio = $2, json = $3 where path = $1"), "update calls set audio = ?2, json = ?3 where path = ?1");
        assert_eq!(question_marks("select '$x', $10"), "select '$x', ?10");
        let mut p = Params::default();
        assert_eq!((p.add(1i64), p.add("a")), ("$1".to_string(), "$2".to_string()));
    }
}
