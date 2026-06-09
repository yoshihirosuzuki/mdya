//! Two-stage incremental check.
//!
//! For each `(collection, path)` pair the writer needs to decide whether
//! to (a) skip entirely, (b) just bump `modified_at` to the new mtime so
//! the next run skips faster, or (c) re-chunk + re-embed + re-insert.
//! The decision is data, not control flow — `Action::decide` is the
//! single place that owns this logic, exhaustively unit-tested below.

use chrono::{DateTime, Utc};

/// Result of the per-file incremental decision. The variant names match
/// the verbs the writer performs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `New` files are not in the DB yet → run the full chunk + embed +
    /// insert path. Bumped into `UpdateSummary.new`.
    New,
    /// `Skip` files are unchanged since the last run by mtime alone →
    /// no FS read, no DB write. Bumped into `UpdateSummary.skipped`.
    Skip,
    /// `TouchMtime` files survived an mtime change but the content hash
    /// still matches the DB value → only the `modified_at` column is
    /// updated; chunks / embedding stay. Bumped into
    /// `UpdateSummary.skipped` (the chunks did not change).
    TouchMtime,
    /// `Reingest` files have a new content hash → delete the old chunks
    /// and insert fresh ones. Bumped into `UpdateSummary.updated`.
    Reingest,
}

/// `DbRow` is the minimum the incremental decision needs from the
/// existing chunks row for a given `(collection, path)`. Wrapping it in
/// a struct (instead of a `(DateTime, String)` tuple) keeps the call
/// site self-documenting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbRow {
    pub modified_at: DateTime<Utc>,
    pub source_hash: String,
}

impl Action {
    /// Decide the action for one file. `fs_mtime` and `fs_hash` describe
    /// the file as it is on disk **now**; `db_row` is what the most
    /// recent ingest stored for the same `(collection, path)`, or
    /// `None` for a brand-new file.
    pub fn decide(fs_mtime: DateTime<Utc>, fs_hash: &str, db_row: Option<&DbRow>) -> Self {
        let Some(row) = db_row else {
            return Action::New;
        };
        if row.modified_at == fs_mtime {
            return Action::Skip;
        }
        if row.source_hash == fs_hash {
            return Action::TouchMtime;
        }
        Action::Reingest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(unix: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(unix, 0).single().expect("valid ts")
    }

    fn db_row(unix: i64, hash: &str) -> DbRow {
        DbRow {
            modified_at: ts(unix),
            source_hash: hash.to_string(),
        }
    }

    #[test]
    fn no_existing_row_means_new_file() {
        let act = Action::decide(ts(100), "abc", None);
        assert_eq!(act, Action::New);
    }

    #[test]
    fn mtime_unchanged_means_skip_regardless_of_hash() {
        // The mtime guard fires first and avoids reading the file at all.
        // Even if the hash would have matched, we don't pay the I/O cost.
        let act = Action::decide(ts(100), "anything", Some(&db_row(100, "stored")));
        assert_eq!(act, Action::Skip);
    }

    #[test]
    fn mtime_changed_but_hash_same_means_touch_mtime() {
        // `git checkout` / `touch` style mtime bump on identical content.
        let act = Action::decide(ts(200), "same", Some(&db_row(100, "same")));
        assert_eq!(act, Action::TouchMtime);
    }

    #[test]
    fn mtime_and_hash_both_changed_means_reingest() {
        let act = Action::decide(ts(200), "new-hash", Some(&db_row(100, "old-hash")));
        assert_eq!(act, Action::Reingest);
    }
}
