use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use codey_runtime_core::codex_sqlite::codex_session_db_paths_from_home;
use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use crate::sqlite_util::table_columns;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubagentStateCleanupReport {
    pub databases_checked: usize,
    pub edges_closed: usize,
    pub jobs_interrupted: usize,
    pub items_interrupted: usize,
    pub assignments_released: usize,
}

/// A Codex child process cannot survive a full desktop restart. Once the old
/// runtime has stopped, every persisted `open` spawn edge is therefore stale.
pub fn close_stale_spawn_edges(home: &Path) -> Result<SubagentStateCleanupReport> {
    let mut report = SubagentStateCleanupReport::default();
    for database_path in database_candidates(home) {
        if !database_path.is_file() {
            continue;
        }
        report.databases_checked += 1;
        let database_report = close_stale_spawn_edges_in_database(&database_path)?;
        report.edges_closed += database_report.edges_closed;
        report.jobs_interrupted += database_report.jobs_interrupted;
        report.items_interrupted += database_report.items_interrupted;
        report.assignments_released += database_report.assignments_released;
    }
    Ok(report)
}

fn database_candidates(home: &Path) -> Vec<PathBuf> {
    let mut paths = codex_session_db_paths_from_home(home);
    // Keep the fixed modern location even when a partial database does not yet
    // contain one of the session tables used by generic database discovery.
    paths.push(home.join("sqlite").join("state_5.sqlite"));
    paths.sort();
    paths.dedup();
    paths
}

fn close_stale_spawn_edges_in_database(path: &Path) -> Result<SubagentStateCleanupReport> {
    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .with_context(|| format!("打开 Codex 子代理状态数据库失败：{}", path.display()))?;
    connection.busy_timeout(Duration::from_secs(5))?;
    let edge_columns = table_columns(&connection, "thread_spawn_edges")?;
    let job_columns = table_columns(&connection, "agent_jobs")?;
    let item_columns = table_columns(&connection, "agent_job_items")?;
    if !edge_columns.contains("status")
        && !job_columns.contains("status")
        && !(item_columns.contains("status") && item_columns.contains("assigned_thread_id"))
    {
        return Ok(SubagentStateCleanupReport::default());
    }

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let edges_closed = if edge_columns.contains("status") {
        transaction.execute(
            "UPDATE thread_spawn_edges
             SET status = 'closed'
             WHERE status IN ('open', 'running', 'active', 'pending', 'pendingInit',
                              'processing', 'inProgress')",
            [],
        )?
    } else {
        0
    };
    let jobs_interrupted = if job_columns.contains("status") {
        transaction.execute(
            "UPDATE agent_jobs
             SET status = 'interrupted'
             WHERE status IN ('open', 'running', 'active', 'pending', 'pendingInit',
                              'processing', 'inProgress')",
            [],
        )?
    } else {
        0
    };
    let (items_interrupted, assignments_released) =
        if item_columns.contains("status") && item_columns.contains("assigned_thread_id") {
            let changed = transaction.execute(
                "UPDATE agent_job_items
                 SET status = 'interrupted', assigned_thread_id = NULL
                 WHERE assigned_thread_id IS NOT NULL
                   AND status IN ('open', 'running', 'active', 'pending', 'pendingInit',
                                  'processing', 'inProgress')",
                [],
            )?;
            (changed, changed)
        } else {
            (0, 0)
        };
    transaction.commit()?;
    Ok(SubagentStateCleanupReport {
        databases_checked: 0,
        edges_closed,
        jobs_interrupted,
        items_interrupted,
        assignments_released,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn create_spawn_edge_database(path: &Path, statuses: &[&str]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE thread_spawn_edges (
                    parent_thread_id TEXT NOT NULL,
                    child_thread_id TEXT NOT NULL,
                    status TEXT NOT NULL
                );",
            )
            .unwrap();
        for (index, status) in statuses.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO thread_spawn_edges VALUES (?1, ?2, ?3)",
                    (format!("parent-{index}"), format!("child-{index}"), status),
                )
                .unwrap();
        }
    }

    fn create_job_database(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE agent_jobs (id TEXT PRIMARY KEY, status TEXT NOT NULL);
                 CREATE TABLE agent_job_items (
                    job_id TEXT NOT NULL,
                    item_id TEXT NOT NULL,
                    status TEXT NOT NULL,
                    assigned_thread_id TEXT,
                    PRIMARY KEY (job_id, item_id)
                 );",
            )
            .unwrap();
        connection
            .execute("INSERT INTO agent_jobs VALUES ('job-1', 'running')", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO agent_job_items VALUES ('job-1', 'item-1', 'inProgress', 'child-1')",
                [],
            )
            .unwrap();
    }

    #[test]
    fn closes_open_edges_and_preserves_existing_terminal_statuses() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("state_5.sqlite");
        create_spawn_edge_database(
            &path,
            &[
                "open",
                "running",
                "active",
                "pendingInit",
                "closed",
                "interrupted",
            ],
        );

        let report = close_stale_spawn_edges(home.path()).unwrap();

        assert_eq!(report.databases_checked, 1);
        assert_eq!(report.edges_closed, 4);
        assert_eq!(report.jobs_interrupted, 0);
        let connection = Connection::open(path).unwrap();
        let statuses = connection
            .prepare("SELECT status FROM thread_spawn_edges ORDER BY child_thread_id")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            statuses,
            [
                "closed",
                "closed",
                "closed",
                "closed",
                "closed",
                "interrupted"
            ]
        );
    }

    #[test]
    fn missing_database_or_table_is_a_noop() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            close_stale_spawn_edges(home.path()).unwrap(),
            SubagentStateCleanupReport::default()
        );

        let connection = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        connection
            .execute("CREATE TABLE threads (id TEXT)", [])
            .unwrap();
        drop(connection);

        assert_eq!(
            close_stale_spawn_edges(home.path()).unwrap(),
            SubagentStateCleanupReport {
                databases_checked: 1,
                edges_closed: 0,
                ..Default::default()
            }
        );
    }

    #[test]
    fn cleans_legacy_and_modern_state_database_locations() {
        let home = tempfile::tempdir().unwrap();
        create_spawn_edge_database(&home.path().join("state_5.sqlite"), &["open"]);
        create_spawn_edge_database(
            &home.path().join("sqlite").join("state_5.sqlite"),
            &["open"],
        );

        let report = close_stale_spawn_edges(home.path()).unwrap();

        assert_eq!(report.databases_checked, 2);
        assert_eq!(report.edges_closed, 2);
    }

    #[test]
    fn interrupts_active_jobs_and_releases_thread_assignments() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("sqlite").join("state_5.sqlite");
        create_job_database(&path);

        let report = close_stale_spawn_edges(home.path()).unwrap();

        assert_eq!(report.databases_checked, 1);
        assert_eq!(report.jobs_interrupted, 1);
        assert_eq!(report.items_interrupted, 1);
        assert_eq!(report.assignments_released, 1);
        let connection = Connection::open(path).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT status FROM agent_jobs", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "interrupted"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT status || ':' || COALESCE(assigned_thread_id, '') FROM agent_job_items",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "interrupted:"
        );
    }
}
