use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use codey_runtime_core::codex_sqlite::codex_session_db_paths_from_home;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::session_index_cleanup;
use crate::sqlite_util::table_columns;

const TOMBSTONE_VERSION: u32 = 1;
const TOMBSTONE_DIR: &str = ".codey-session-delete-tombstones-v1";
const SESSION_DIRS: [&str; 2] = ["sessions", "archived_sessions"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionDeleteTombstone {
    version: u32,
    session_id: String,
    title: String,
    deleted_at_ms: u128,
}

#[derive(Debug, Default)]
pub(crate) struct ReplaySummary {
    pub sessions: usize,
    pub database_rows: usize,
    pub rollout_files: usize,
    pub index_entries: usize,
    pub failures: Vec<(String, String)>,
}

pub fn record(home: &Path, session_id: &str, title: &str) -> Result<()> {
    let session_id = crate::session_metadata::normalize_session_id(session_id)
        .trim()
        .to_string();
    if session_id.is_empty() {
        anyhow::bail!("会话 ID 不能为空");
    }
    let directory = tombstone_directory(home);
    fs::create_dir_all(&directory)
        .with_context(|| format!("创建会话删除墓碑目录失败：{}", directory.display()))?;
    let path = tombstone_path(&directory, &session_id);
    if path.exists() {
        return Ok(());
    }
    let tombstone = SessionDeleteTombstone {
        version: TOMBSTONE_VERSION,
        session_id,
        title: title.trim().to_string(),
        deleted_at_ms: crate::fs_util::timestamp_millis(),
    };
    let temp = crate::fs_util::unique_temp_path(&path);
    fs::write(&temp, serde_json::to_vec(&tombstone)?)
        .with_context(|| format!("写入会话删除墓碑失败：{}", temp.display()))?;
    crate::fs_util::persist_temp_file(&temp, &path)
        .with_context(|| format!("保存会话删除墓碑失败：{}", path.display()))
}

/// Explicit import is the one operation allowed to intentionally reuse a
/// deleted ID. It clears only that exact tombstone after the import commits.
pub fn clear(home: &Path, session_id: &str) -> Result<()> {
    let session_id = crate::session_metadata::normalize_session_id(session_id);
    if session_id.is_empty() {
        return Ok(());
    }
    let path = tombstone_path(&tombstone_directory(home), session_id);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("清理会话删除墓碑失败：{}", path.display()))
        }
    }
}

pub(crate) fn replay(home: &Path) -> Result<ReplaySummary> {
    let tombstones = read_tombstones(home)?;
    let mut summary = ReplaySummary::default();
    for tombstone in tombstones {
        summary.sessions += 1;
        match scrub_session(home, &tombstone.session_id) {
            Ok((rows, rollouts)) => {
                summary.database_rows += rows;
                summary.rollout_files += rollouts;
                match session_index_cleanup::remove_thread(home, &tombstone.session_id) {
                    Ok(report) => summary.index_entries += report.pruned_entries,
                    Err(error) => summary.failures.push((
                        tombstone.session_id.clone(),
                        format!("清理会话索引失败：{error:#}"),
                    )),
                }
            }
            Err(error) => summary.failures.push((
                tombstone.session_id.clone(),
                format!("重放删除墓碑失败：{error:#}"),
            )),
        }
    }
    Ok(summary)
}

fn read_tombstones(home: &Path) -> Result<Vec<SessionDeleteTombstone>> {
    let directory = tombstone_directory(home);
    let Ok(entries) = fs::read_dir(&directory) else {
        return Ok(Vec::new());
    };
    let mut result = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(OsStr::to_str) != Some("json") {
            continue;
        }
        let tombstone: SessionDeleteTombstone = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("解析会话删除墓碑失败：{}", path.display()))?;
        if tombstone.version != TOMBSTONE_VERSION || tombstone.session_id.trim().is_empty() {
            anyhow::bail!("无效的会话删除墓碑：{}", path.display());
        }
        result.push(tombstone);
    }
    result.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    Ok(result)
}

fn scrub_session(home: &Path, session_id: &str) -> Result<(usize, usize)> {
    let mut db_paths = codex_session_db_paths_from_home(home);
    let sqlite_dir = home.join("sqlite");
    if let Ok(entries) = fs::read_dir(sqlite_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && matches!(
                    path.extension().and_then(OsStr::to_str),
                    Some("db" | "sqlite" | "sqlite3")
                )
            {
                db_paths.push(path);
            }
        }
    }
    let legacy = home.join("state_5.sqlite");
    if legacy.is_file() {
        db_paths.push(legacy);
    }
    db_paths.sort();
    db_paths.dedup();

    let mut rows = 0usize;
    let mut rollout_paths = HashSet::new();
    for path in db_paths {
        if !path.exists() {
            continue;
        }
        let mut db = Connection::open(&path)
            .with_context(|| format!("打开会话数据库失败：{}", path.display()))?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        let tables = [
            ("threads", "id", true),
            ("local_thread_catalog", "thread_id", false),
            ("automation_runs", "thread_id", false),
            ("inbox_items", "thread_id", false),
            ("sessions", "id", false),
            ("messages", "session_id", false),
            ("thread_dynamic_tools", "thread_id", false),
            ("thread_goals", "thread_id", false),
            ("stage1_outputs", "thread_id", false),
            ("thread_spawn_edges", "parent_thread_id", false),
            ("thread_spawn_edges", "child_thread_id", false),
            ("agent_jobs", "thread_id", false),
        ];
        let tx = db.transaction()?;
        for (table, column, is_thread_table) in tables {
            let columns = table_columns(&tx, table)?;
            if !columns.contains(column) {
                continue;
            }
            if is_thread_table && columns.contains("rollout_path") {
                let mut statement = tx.prepare(&format!(
                    "SELECT rollout_path FROM {table} WHERE {column}=?1"
                ))?;
                let paths = statement
                    .query_map(params![session_id], |row| row.get::<_, Option<String>>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(statement);
                for path in paths
                    .into_iter()
                    .flatten()
                    .filter(|path| !path.trim().is_empty())
                {
                    rollout_paths.insert(PathBuf::from(path));
                }
            }
            let sql = format!("DELETE FROM {table} WHERE {column}=?1");
            rows += tx.execute(&sql, params![session_id])?;
        }
        if table_columns(&tx, "agent_job_items")?.contains("assigned_thread_id") {
            rows += tx.execute(
                "UPDATE agent_job_items SET assigned_thread_id=NULL WHERE assigned_thread_id=?1",
                params![session_id],
            )?;
        }
        tx.commit()?;
    }

    let mut removed_rollouts = 0;
    for path in rollout_files(home)? {
        if rollout_matches_session(&path, session_id)? {
            if remove_rollout_file(home, &path)? {
                removed_rollouts += 1;
            }
        }
    }
    for path in rollout_paths {
        if let Some(path) = safe_rollout_path(home, &path)?
            && fs::remove_file(path).is_ok()
        {
            removed_rollouts += 1;
        }
    }
    Ok((rows, removed_rollouts))
}

fn rollout_files(home: &Path) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    for dirname in SESSION_DIRS {
        let root = home.join(dirname);
        if fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.file_type().is_dir()) {
            collect_rollout_files(&root, &mut result)?;
        }
    }
    Ok(result)
}

fn collect_rollout_files(root: &Path, result: &mut Vec<PathBuf>) -> Result<()> {
    for entry in
        fs::read_dir(root).with_context(|| format!("扫描会话目录失败：{}", root.display()))?
    {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("读取会话路径元数据失败：{}", path.display()))?;
        if metadata.file_type().is_dir() {
            collect_rollout_files(&path, result)?;
        } else if metadata.file_type().is_file()
            && path
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
        {
            result.push(path);
        }
    }
    Ok(())
}

fn remove_rollout_file(home: &Path, path: &Path) -> Result<bool> {
    let Some(path) = safe_rollout_path(home, path)? else {
        return Ok(false);
    };
    Ok(fs::remove_file(path).is_ok())
}

/// Resolve a rollout reference before deletion. SQLite may contain paths
/// written by another synchronizer, including relative paths and symlinks;
/// only a real file whose resolved target remains under Codex home is eligible.
fn safe_rollout_path(home: &Path, rollout_path: &Path) -> Result<Option<PathBuf>> {
    let canonical_home = home
        .canonicalize()
        .with_context(|| format!("找不到 Codex 数据目录：{}", home.display()))?;
    let candidate = if rollout_path.is_absolute() {
        rollout_path.to_path_buf()
    } else {
        home.join(rollout_path)
    };
    let metadata = match fs::symlink_metadata(&candidate) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取会话记录元数据失败：{}", candidate.display()));
        }
    };
    if !metadata.file_type().is_file() {
        return Ok(None);
    }
    let canonical = match candidate.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("解析会话记录路径失败：{}", candidate.display()));
        }
    };
    if canonical.starts_with(&canonical_home) {
        Ok(Some(canonical))
    } else {
        Ok(None)
    }
}

fn rollout_matches_session(path: &Path, session_id: &str) -> Result<bool> {
    if let Some(name) = path.file_name().and_then(OsStr::to_str) {
        let stem = name
            .strip_prefix("rollout-")
            .and_then(|value| value.strip_suffix(".jsonl"));
        if let Some(stem) = stem {
            if stem.ends_with(session_id) {
                return Ok(true);
            }
        }
    }
    let bytes = fs::read(path).with_context(|| format!("读取会话记录失败：{}", path.display()))?;
    for line in String::from_utf8_lossy(&bytes).lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        let id = record
            .pointer("/payload/id")
            .and_then(Value::as_str)
            .map(crate::session_metadata::normalize_session_id);
        return Ok(id == Some(session_id));
    }
    Ok(false)
}

fn tombstone_directory(home: &Path) -> PathBuf {
    home.join(TOMBSTONE_DIR)
}

fn tombstone_path(directory: &Path, session_id: &str) -> PathBuf {
    let digest = Sha256::digest(session_id.as_bytes());
    directory.join(format!("{digest:x}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use tempfile::tempdir;

    #[test]
    fn replay_removes_a_recreated_thread_and_keeps_the_tombstone() {
        let home = tempdir().unwrap();
        record(home.path(), "local:s1", "deleted").unwrap();
        let sessions = home.path().join("sessions/2026/08/26");
        fs::create_dir_all(&sessions).unwrap();
        let rollout = sessions.join("rollout-s1.jsonl");
        fs::write(
            &rollout,
            r#"{"type":"session_meta","payload":{"id":"s1"}}
"#,
        )
        .unwrap();
        let db = home.path().join("state_5.sqlite");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads (id, rollout_path) VALUES (?1, ?2)",
                params!["s1", rollout.to_string_lossy()],
            )
            .unwrap();
        drop(connection);

        let summary = replay(home.path()).unwrap();
        assert_eq!(summary.sessions, 1);
        assert!(!rollout.exists());
        assert_eq!(
            Connection::open(db)
                .unwrap()
                .query_row("SELECT COUNT(*) FROM threads WHERE id='s1'", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            0
        );
        assert_eq!(read_tombstones(home.path()).unwrap().len(), 1);
    }

    #[test]
    fn rejects_rollout_paths_outside_codex_home() {
        let root = tempdir().unwrap();
        let home = root.path().join("codex");
        let outside = root.path().join("outside.jsonl");
        fs::create_dir_all(&home).unwrap();
        fs::write(&outside, b"outside").unwrap();

        assert!(
            safe_rollout_path(&home, Path::new("../outside.jsonl"))
                .unwrap()
                .is_none()
        );
        assert!(safe_rollout_path(&home, &outside).unwrap().is_none());
        assert_eq!(fs::read(&outside).unwrap(), b"outside");
    }

    #[cfg(unix)]
    #[test]
    fn rollout_scan_does_not_follow_symlinked_directories() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let home = root.path().join("codex");
        let outside = root.path().join("outside");
        fs::create_dir_all(home.join("sessions")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let rollout = outside.join("rollout-outside.jsonl");
        fs::write(&rollout, b"outside").unwrap();
        symlink(&outside, home.join("sessions/linked")).unwrap();

        let files = rollout_files(&home).unwrap();
        assert!(files.is_empty());
        assert!(rollout.exists());
    }
}
