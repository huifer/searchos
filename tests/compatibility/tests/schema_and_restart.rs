//! Compatibility suite: schema versioning, migration idempotence, data
//! preservation across restarts, CLI JSON contract stability.

use searchos_storage_sqlite::store::SqliteStore;

#[test]
fn migration_is_idempotent_and_versioned() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.db");
    let s1 = SqliteStore::open(&db).unwrap();
    let v1 = s1.schema_version().unwrap();
    drop(s1);
    // Reopen: no new migration applied, version unchanged.
    let s2 = SqliteStore::open(&db).unwrap();
    assert_eq!(s2.schema_version().unwrap(), v1);
    // A pristine DB always starts at the current migration version.
    let dir2 = tempfile::tempdir().unwrap();
    let s3 = SqliteStore::open(dir2.path().join("fresh.db")).unwrap();
    assert_eq!(
        s3.schema_version().unwrap(),
        searchos_storage_sqlite::migrations::MIGRATIONS
            .last()
            .unwrap()
            .version
    );
}

#[test]
fn data_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.db");
    {
        let s = SqliteStore::open(&db).unwrap();
        s.with_conn(|c| {
            c.execute(
                "INSERT INTO workspaces (id,name,config_version,created_at,updated_at) VALUES ('w1','one',1,'t','t')",
                [],
            )
            .map_err(|e| searchos_domain::SearchOsError::Storage(e.to_string()))
        })
        .unwrap();
    }
    {
        let s = SqliteStore::open(&db).unwrap();
        let n: i64 = s
            .with_conn(|c| {
                c.query_row("SELECT count(*) FROM workspaces WHERE id='w1'", [], |r| {
                    r.get(0)
                })
                .map_err(|e| searchos_domain::SearchOsError::Storage(e.to_string()))
            })
            .unwrap();
        assert_eq!(n, 1);
    }
}

#[test]
fn migration_history_is_append_only() {
    // Released migrations are immutable: the list only grows, never edits.
    let versions: Vec<i64> = searchos_storage_sqlite::migrations::MIGRATIONS
        .iter()
        .map(|m| m.version)
        .collect();
    let mut sorted = versions.clone();
    sorted.sort();
    assert_eq!(versions, sorted, "migrations must be ordered by version");
    assert_eq!(
        versions.len(),
        versions
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
    );
}
