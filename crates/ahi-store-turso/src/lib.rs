//! A synchronous SQLite-format database for ahi, over Turso.
//!
//! [Turso](https://github.com/tursodatabase/turso) is a re-implementation of
//! SQLite in Rust: the file it writes opens in the `sqlite3` CLI, there is
//! no C in the build, and it runs on every platform ahi does. Its API is
//! `async`; ahi has no async runtime and wants none, so this crate drives
//! the futures with a twelve-line executor and exposes plain calls that
//! return when the work is done. Nothing Turso-shaped crosses this
//! boundary: callers see [`Database`], [`Connection`], [`Value`], and
//! [`Error`].
//!
//! This is the workspace's one owner of the `turso` dependency. The
//! `SessionStore` logic lives in `ahi`, where it is tested against the JSONL
//! backend with the same suite; this crate is the engine only.
//!
//! One rule the engine imposes: Turso takes a process-scoped lock on the
//! database file while it is open, so two processes cannot hold the same
//! database at once. Callers keep a database open only for one operation
//! and retry an `Error::Locked` for a bounded time.

use std::future::Future;
use std::path::Path;
use std::pin::pin;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

/// One SQL value, as a row holds it.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }
}

impl From<&str> for Value {
    fn from(text: &str) -> Self {
        Self::Text(text.to_string())
    }
}

impl From<String> for Value {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<Option<String>> for Value {
    fn from(text: Option<String>) -> Self {
        text.map_or(Self::Null, Self::Text)
    }
}

impl From<Option<&str>> for Value {
    fn from(text: Option<&str>) -> Self {
        text.map_or(Self::Null, |text| Self::Text(text.to_string()))
    }
}

/// Why a call failed. The text is the engine's, which never contains row
/// data for the statements ahi runs (parameters are bound, never inlined).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Another process holds the database file. Retry later.
    Locked(String),
    /// The statement is wrong for the schema: a bug, not a condition.
    Sql(String),
    /// The file could not be opened, read, or written.
    Io(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Locked(message) | Self::Sql(message) | Self::Io(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn classify(error: turso::Error) -> Error {
    let message = error.to_string();
    let lower = message.to_ascii_lowercase();
    if lower.contains("locked") || lower.contains("busy") {
        Error::Locked(message)
    } else if lower.contains("syntax")
        || lower.contains("no such")
        || lower.contains("constraint")
        || lower.contains("column")
        || lower.contains("parse")
    {
        Error::Sql(message)
    } else {
        Error::Io(message)
    }
}

/// Drive a future to completion on this thread. Turso's futures complete
/// through its own I/O loop, not through a reactor, so polling in a loop
/// with a no-op waker is the whole executor. `yield_now` keeps a spin from
/// starving the thread that owns the file descriptor.
fn block_on<F: Future>(future: F) -> F::Output {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        std::thread::yield_now();
    }
}

/// An open database file (or an in-memory one for tests).
pub struct Database {
    inner: turso::Database,
}

impl Database {
    /// Open or create the file at `path`. Fails with [`Error::Locked`] while
    /// another process holds it.
    pub fn open(path: &Path) -> Result<Self> {
        let path = path
            .to_str()
            .ok_or_else(|| Error::Io(format!("{}: path is not UTF-8", path.display())))?;
        let inner = block_on(turso::Builder::new_local(path).build()).map_err(classify)?;
        Ok(Self { inner })
    }

    /// A database that lives only in this process.
    pub fn memory() -> Result<Self> {
        let inner = block_on(turso::Builder::new_local(":memory:").build()).map_err(classify)?;
        Ok(Self { inner })
    }

    pub fn connect(&self) -> Result<Connection> {
        let inner = self.inner.connect().map_err(classify)?;
        // A writer that finds the database busy inside this process waits
        // briefly instead of failing at once.
        let _ = inner.busy_timeout(Duration::from_millis(500));
        Ok(Connection { inner })
    }
}

pub struct Connection {
    inner: turso::Connection,
}

impl Connection {
    /// Run one statement that returns no rows. Returns the rows affected.
    pub fn execute(&self, sql: &str, params: &[Value]) -> Result<u64> {
        block_on(self.inner.execute(sql, to_params(params))).map_err(classify)
    }

    /// Run several statements separated by `;`, such as a schema.
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        block_on(self.inner.execute_batch(sql)).map_err(classify)
    }

    /// Run a query and collect every row.
    pub fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Vec<Value>>> {
        block_on(async {
            let mut rows = self.inner.query(sql, to_params(params)).await?;
            let mut collected = Vec::new();
            while let Some(row) = rows.next().await? {
                let mut values = Vec::with_capacity(row.column_count());
                for index in 0..row.column_count() {
                    values.push(from_turso(row.get_value(index)?));
                }
                collected.push(values);
            }
            Ok(collected)
        })
        .map_err(classify)
    }

    /// `execute` a sequence of statements inside one transaction: all land
    /// or none do. A failure rolls back and is returned.
    pub fn transaction(&self, statements: &[(&str, Vec<Value>)]) -> Result<()> {
        self.execute("BEGIN", &[])?;
        for (sql, params) in statements {
            if let Err(error) = self.execute(sql, params) {
                let _ = self.execute("ROLLBACK", &[]);
                return Err(error);
            }
        }
        self.execute("COMMIT", &[]).map(|_| ())
    }
}

fn to_params(params: &[Value]) -> Vec<turso::Value> {
    params
        .iter()
        .map(|value| match value {
            Value::Null => turso::Value::Null,
            Value::Integer(value) => turso::Value::Integer(*value),
            Value::Real(value) => turso::Value::Real(*value),
            Value::Text(text) => turso::Value::Text(text.clone()),
            Value::Blob(bytes) => turso::Value::Blob(bytes.clone()),
        })
        .collect()
}

fn from_turso(value: turso::Value) -> Value {
    match value {
        turso::Value::Null => Value::Null,
        turso::Value::Integer(value) => Value::Integer(value),
        turso::Value::Real(value) => Value::Real(value),
        turso::Value::Text(text) => Value::Text(text),
        turso::Value::Blob(bytes) => Value::Blob(bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_run_synchronously_and_transactions_are_atomic() {
        let database = Database::memory().unwrap();
        let connection = database.connect().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE t(id TEXT PRIMARY KEY, n INTEGER); CREATE INDEX t_n ON t(n);",
            )
            .unwrap();
        assert_eq!(
            connection
                .execute("INSERT INTO t VALUES (?1, ?2)", &["a".into(), 1.into()])
                .unwrap(),
            1
        );
        // The second insert violates the key: the first in the batch must
        // not survive.
        let failed = connection.transaction(&[
            ("INSERT INTO t VALUES (?1, ?2)", vec!["b".into(), 2.into()]),
            ("INSERT INTO t VALUES (?1, ?2)", vec!["a".into(), 3.into()]),
        ]);
        assert!(matches!(failed, Err(Error::Sql(_))), "{failed:?}");
        let rows = connection
            .query("SELECT id, n FROM t ORDER BY id", &[])
            .unwrap();
        assert_eq!(rows, vec![vec![Value::Text("a".into()), Value::Integer(1)]]);
        connection
            .transaction(&[
                ("INSERT INTO t VALUES (?1, ?2)", vec!["b".into(), 2.into()]),
                ("UPDATE t SET n = n + 10 WHERE id = ?1", vec!["a".into()]),
            ])
            .unwrap();
        let rows = connection
            .query("SELECT id, n FROM t WHERE n > ?1 ORDER BY id", &[1.into()])
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][1].as_i64(), Some(11));
        assert_eq!(rows[1][0].as_str(), Some("b"));
        let nulls = connection
            .query("SELECT NULL, ?1", &[Value::from(None::<String>)])
            .unwrap();
        assert_eq!(nulls, vec![vec![Value::Null, Value::Null]]);
    }

    #[test]
    fn a_file_database_is_readable_again_after_it_is_dropped() {
        let dir = std::env::temp_dir().join(format!("ahi-store-turso-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        {
            let database = Database::open(&path).unwrap();
            let connection = database.connect().unwrap();
            connection
                .execute_batch("CREATE TABLE t(id TEXT); INSERT INTO t VALUES ('x');")
                .unwrap();
        }
        let reopened = Database::open(&path).unwrap();
        let rows = reopened
            .connect()
            .unwrap()
            .query("SELECT id FROM t", &[])
            .unwrap();
        assert_eq!(rows, vec![vec![Value::Text("x".into())]]);
        drop(reopened);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
