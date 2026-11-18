//! Data layer for the panel.
//!
//! The DB is opened strictly read-only through a `file:...?mode=ro` URI. Nothing
//! in this module can create, migrate or write to the database.

use std::path::Path;

use anyhow::{bail, Context, Result};
use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection, OpenFlags};

/// The only sort keys accepted by the panel. Mirrors SORT_CONTRACT.md.
pub const SORT_KEYS: [&str; 5] = ["type", "anonymity", "latency", "country", "score"];

/// Columns the panel needs in the `proxies` table. An older schema must fail
/// loudly rather than render blanks.
pub const REQUIRED_COLUMNS: [&str; 9] = [
    "ip",
    "port",
    "type",
    "country",
    "anonymity",
    "latency_ms",
    "honeypot_score",
    "alive",
    "last_check",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Asc,
    Desc,
}

impl Dir {
    pub fn as_str(self) -> &'static str {
        match self {
            Dir::Asc => "asc",
            Dir::Desc => "desc",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "asc" => Ok(Dir::Asc),
            "desc" => Ok(Dir::Desc),
            other => bail!("unknown direction `{other}` (expected `asc` or `desc`)"),
        }
    }

    pub fn flip(self) -> Self {
        match self {
            Dir::Asc => Dir::Desc,
            Dir::Desc => Dir::Asc,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Filters {
    /// `None` means all types.
    pub type_filter: Option<String>,
    /// `None` means all anonymity levels.
    pub anonymity: Option<String>,
    /// Case-insensitive substring match against the proxy IP.
    pub ip_search: String,
}

#[derive(Clone, Debug)]
pub struct Proxy {
    pub ip: String,
    pub port: i64,
    pub proxy_type: String,
    pub country: String,
    pub anonymity: String,
    pub latency_ms: Option<i64>,
    pub honeypot_score: i64,
    pub alive: bool,
}

impl Proxy {
    pub fn ip_port(&self) -> String {
        format!("{}:{}", self.ip, self.port)
    }

    pub fn latency_display(&self) -> String {
        match self.latency_ms {
            Some(ms) if ms > 0 => ms.to_string(),
            _ => "-".to_string(),
        }
    }

    pub fn country_display(&self) -> &str {
        if self.country.is_empty() {
            "-"
        } else {
            &self.country
        }
    }

    pub fn anonymity_display(&self) -> &str {
        if self.anonymity.is_empty() {
            "unknown"
        } else {
            &self.anonymity
        }
    }
}

/// A read-only handle to a harvester database.
pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let uri = format!("file:{}?mode=ro", path.display());
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(&uri, flags)
            .with_context(|| format!("cannot open `{}` read-only", path.display()))?;
        // The harvester may be writing (WAL) while we read. Wait for a lock
        // instead of failing, and make double sure this handle cannot write.
        conn.busy_timeout(std::time::Duration::from_millis(5000))
            .context("cannot set busy_timeout on the read-only handle")?;
        conn.execute_batch("PRAGMA query_only=1;")
            .context("cannot mark the connection query-only")?;
        let store = Store { conn };
        store.validate_schema()?;
        Ok(store)
    }

    /// Fail with the name of the first expected column that is absent.
    pub fn validate_schema(&self) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare("PRAGMA table_info(proxies)")
            .context("cannot inspect the proxies table")?;
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        if columns.is_empty() {
            bail!("database has no `proxies` table (or it has no columns)");
        }

        for required in REQUIRED_COLUMNS {
            if !columns.iter().any(|name| name.eq_ignore_ascii_case(required)) {
                bail!(
                    "proxies table is missing required column `{required}` (found: {})",
                    columns.join(", ")
                );
            }
        }
        Ok(())
    }

    /// Load rows in the exact order defined by SORT_CONTRACT.md.
    ///
    /// `sort` and `dir` are matched against fixed canned fragments; they are
    /// never interpolated from arbitrary input. Filter values are bound.
    pub fn sorted(&self, sort: &str, dir: Dir, filters: &Filters) -> Result<Vec<Proxy>> {
        let order = order_clause(sort, dir)?;

        let mut sql = String::from(
            "SELECT ip, port, type, COALESCE(country, ''), \
             COALESCE(anonymity, ''), latency_ms, COALESCE(honeypot_score, 0), alive \
             FROM proxies WHERE 1 = 1",
        );
        let mut values: Vec<Value> = Vec::new();

        if let Some(kind) = filters.type_filter.as_deref() {
            sql.push_str(" AND type = ?");
            values.push(Value::Text(kind.to_string()));
        }
        if let Some(anonymity) = filters.anonymity.as_deref() {
            sql.push_str(" AND anonymity = ?");
            values.push(Value::Text(anonymity.to_string()));
        }
        if !filters.ip_search.is_empty() {
            sql.push_str(" AND ip LIKE ?");
            values.push(Value::Text(format!("%{}%", filters.ip_search)));
        }

        sql.push(' ');
        sql.push_str(&order);

        let mut stmt = self
            .conn
            .prepare(&sql)
            .context("cannot prepare the proxy query")?;
        let rows = stmt.query_map(params_from_iter(values.iter()), |row| {
            Ok(Proxy {
                ip: row.get(0)?,
                port: row.get(1)?,
                proxy_type: row.get(2)?,
                country: row.get(3)?,
                anonymity: row.get(4)?,
                latency_ms: row.get(5)?,
                honeypot_score: row.get(6)?,
                alive: row.get::<_, i64>(7)? != 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

/// Build a safe ORDER BY clause for one of the five contract keys.
fn order_clause(sort: &str, dir: Dir) -> Result<String> {
    let d = dir.as_str();
    let clause = match sort {
        // Plain ASC/DESC: descending strings are ordinary reverse alphabetical
        // order, so `https` comes before `http`.
        "type" => format!("type {d}"),
        "anonymity" => format!(
            "CASE anonymity WHEN 'elite' THEN 3 WHEN 'anonymous' THEN 2 \
             WHEN 'transparent' THEN 1 ELSE 0 END {d}"
        ),
        // Unknown latency (NULL or <= 0) sorts last in BOTH directions, and
        // all unknown rows tie on 0 so the ip/port tie-break stays stable.
        "latency" => format!(
            "CASE WHEN latency_ms IS NULL OR latency_ms <= 0 THEN 1 ELSE 0 END ASC, \
             CASE WHEN latency_ms IS NULL OR latency_ms <= 0 THEN 0 ELSE latency_ms END {d}"
        ),
        // Missing country (NULL or '') sorts last in BOTH directions.
        "country" => format!(
            "CASE WHEN country IS NULL OR country = '' THEN 1 ELSE 0 END ASC, \
             UPPER(COALESCE(country, '')) {d}"
        ),
        "score" => format!("COALESCE(honeypot_score, 0) {d}"),
        other => bail!(
            "unknown sort key `{other}` (expected one of: {})",
            SORT_KEYS.join(", ")
        ),
    };
    Ok(format!("ORDER BY {clause}, ip ASC, port ASC"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bogus_sort_key_is_rejected() {
        let err = order_clause("definitely-not-a-key", Dir::Asc).unwrap_err();
        assert!(err.to_string().contains("unknown sort key"));
    }

    #[test]
    fn descending_string_keys_are_plain_reverse_alphabetical() {
        let clause = order_clause("type", Dir::Desc).unwrap();
        assert_eq!(clause.to_lowercase(), "order by type desc, ip asc, port asc");
        assert!(!clause.contains("sort_desc_key"));
        assert!(!order_clause("country", Dir::Desc)
            .unwrap()
            .contains("sort_desc_key"));
    }

    #[test]
    fn unknown_latency_sorts_last_in_both_directions() {
        let asc = order_clause("latency", Dir::Asc).unwrap();
        let desc = order_clause("latency", Dir::Desc).unwrap();
        assert!(asc.starts_with(
            "ORDER BY CASE WHEN latency_ms IS NULL OR latency_ms <= 0 THEN 1 ELSE 0 END ASC"
        ));
        assert!(desc.starts_with(
            "ORDER BY CASE WHEN latency_ms IS NULL OR latency_ms <= 0 THEN 1 ELSE 0 END ASC"
        ));
    }
}
