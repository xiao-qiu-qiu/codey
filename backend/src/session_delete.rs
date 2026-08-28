use std::path::Path;

use anyhow::{Result, anyhow};
use codey_runtime_core::codex_sqlite::codex_session_db_paths_from_home;
use codey_runtime_core::models::{DeleteResult, DeleteStatus, SessionRef};
use codey_runtime_data::delete_local_from_paths;

use crate::session_delete_tombstone;
use crate::session_index_cleanup;

pub fn delete_session(home: &Path, session_id: &str, title: &str) -> Result<DeleteResult> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        anyhow::bail!("会话 ID 不能为空");
    }
    let session = SessionRef::new(session_id, title.trim().to_string())?;
    // Persist the deletion before touching storage. A running Codex or an
    // external synchronizer may recreate the old row after this request; the
    // startup replay will remove it again until an explicit import clears it.
    session_delete_tombstone::record(home, &session.session_id, &session.title)?;
    let mut result = delete_local_from_paths(codex_session_db_paths_from_home(home), &session);
    if matches!(result.status, DeleteStatus::LocalDeleted)
        || is_missing_local_result(&result.message)
    {
        // A stale sidebar entry may already have lost its database row. It is
        // still a successful logical deletion: remove every index form and
        // retain the tombstone so a later synchronizer cannot resurrect it.
        if !matches!(result.status, DeleteStatus::LocalDeleted) {
            result.status = DeleteStatus::LocalDeleted;
            result.message = "会话已不存在，已从本地索引移除".to_string();
        }
        if let Err(error) = session_index_cleanup::remove_thread(home, &session.session_id) {
            eprintln!("删除会话后清理 session_index.jsonl 失败，将在下次启动时重试：{error:#}");
            result.message = format!("{}；会话索引将在下次启动时继续清理", result.message);
        }
        Ok(result)
    } else {
        let _ = session_delete_tombstone::clear(home, &session.session_id);
        Err(anyhow!(result.message))
    }
}

fn is_missing_local_result(message: &str) -> bool {
    if message
        .strip_prefix("删除本地会话失败：")
        .is_some_and(|details| {
            details
                .split('；')
                .all(|detail| detail.contains("Database not found:"))
        })
    {
        return true;
    }
    matches!(
        message,
        "Thread not found in local storage"
            | "Session not found in local storage"
            | "Database not found: state_5.sqlite"
    ) || message.starts_with("Database not found:")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::{Connection, params};
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn permanently_deletes_the_thread_catalog_index_and_rollout_without_backups() {
        let home = tempdir().unwrap();
        let rollout_dir = home.path().join("sessions/2026/07/18");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join("rollout-thread-1.jsonl");
        fs::write(&rollout, "{\"type\":\"session_meta\"}\n").unwrap();

        let db_path = home.path().join("state_5.sqlite");
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    rollout_path TEXT NOT NULL,
                    title TEXT NOT NULL
                )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads (id, rollout_path, title) VALUES (?1, ?2, ?3)",
                params!["thread-1", rollout.to_string_lossy(), "待删除会话"],
            )
            .unwrap();
        drop(connection);

        let sqlite_dir = home.path().join("sqlite");
        fs::create_dir_all(&sqlite_dir).unwrap();
        let catalog_path = sqlite_dir.join("codex-dev.db");
        let catalog = Connection::open(&catalog_path).unwrap();
        catalog
            .execute_batch(
                "CREATE TABLE local_thread_catalog (
                    host_id TEXT NOT NULL,
                    thread_id TEXT NOT NULL,
                    display_title TEXT NOT NULL,
                    PRIMARY KEY (host_id, thread_id)
                );
                INSERT INTO local_thread_catalog
                    (host_id, thread_id, display_title)
                VALUES ('local', 'thread-1', '待删除会话');",
            )
            .unwrap();
        drop(catalog);
        fs::write(
            home.path().join("session_index.jsonl"),
            "{\"id\":\"thread-1\",\"thread_name\":\"待删除会话\",\"updated_at\":\"2026-07-24T00:00:00Z\"}\n",
        )
        .unwrap();

        let result = delete_session(home.path(), "local:thread-1", "待删除会话").unwrap();

        assert_eq!(result.status, DeleteStatus::LocalDeleted);
        assert!(result.undo_token.is_none());
        assert!(result.backup_path.is_none());
        assert!(!rollout.exists());
        assert_eq!(
            Connection::open(db_path)
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM threads WHERE id='thread-1'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            Connection::open(catalog_path)
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM local_thread_catalog WHERE thread_id='thread-1'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert!(
            !fs::read_to_string(home.path().join("session_index.jsonl"))
                .unwrap()
                .contains("\"id\":\"thread-1\"")
        );
        assert!(!home.path().join("codey-backups").exists());
        assert!(!home.path().join("backups_state").exists());
    }

    #[test]
    fn rejects_an_empty_session_id() {
        let home = tempdir().unwrap();
        let error = delete_session(home.path(), "  ", "").unwrap_err();
        assert!(error.to_string().contains("会话 ID"));
    }

    #[test]
    fn removes_an_index_only_ghost_and_keeps_a_permanent_tombstone() {
        let home = tempdir().unwrap();
        fs::write(
            home.path().join("session_index.jsonl"),
            serde_json::to_string(&serde_json::json!({
                "threadId": "local:ghost-1",
                "title": "从同步器带回的旧会话",
                "updatedAt": "2026-08-26T00:00:00Z",
                "source": "codex-x",
            }))
            .unwrap()
                + "\n",
        )
        .unwrap();

        let result = delete_session(home.path(), "local:ghost-1", "旧会话").unwrap();

        assert_eq!(result.status, DeleteStatus::LocalDeleted);
        assert!(
            !fs::read_to_string(home.path().join("session_index.jsonl"))
                .unwrap()
                .contains("ghost-1")
        );
        assert!(
            home.path()
                .join(".codey-session-delete-tombstones-v1")
                .read_dir()
                .unwrap()
                .next()
                .is_some()
        );
    }
}
