use super::{index, segment_path, store};
use crate::server::infrastructure::connection_pool::ConnectionPool;
use crate::server::router::resolve_recording_path;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

static BUILD: Mutex<()> = Mutex::const_new(());

fn resolve(root: &Path, stored: &str) -> Option<PathBuf> {
    let path = segment_path(&root.join(stored));
    let relative = path.strip_prefix(root).ok()?.to_str()?;
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(extension.as_str(), "flv" | "ts") {
        return None;
    }
    resolve_recording_path(root, relative).ok().or_else(|| {
        if relative.contains('/') || relative.contains('\\') {
            return None;
        }
        resolve_recording_path(root, &format!("recordings/{relative}")).ok()
    })
}

pub async fn available(pool: &ConnectionPool) -> sqlx::Result<HashSet<i64>> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT s.id, f.file FROM stream_sessions s JOIN filelist f ON f.session_id = s.id
         WHERE s.started_at IS NULL AND s.ended_at IS NOT NULL AND s.deleted_at IS NULL",
    )
    .fetch_all(pool)
    .await?;
    let Ok(root) = std::env::current_dir() else {
        return Ok(HashSet::new());
    };
    Ok(rows
        .into_iter()
        .filter_map(|(id, file)| resolve(&root, &file).map(|_| id))
        .collect())
}

pub async fn prepare(pool: &ConnectionPool, id: i64) -> super::Result<()> {
    if store::session_summary(pool, id).await?.is_some() {
        return Ok(());
    }
    let _guard = BUILD.lock().await;
    if store::session_summary(pool, id).await?.is_some() {
        return Ok(());
    }
    let files: Vec<String> = sqlx::query_scalar(
        "SELECT f.file FROM filelist f JOIN stream_sessions s ON s.id = f.session_id
         WHERE s.id = ? AND s.started_at IS NULL AND s.ended_at IS NOT NULL AND s.deleted_at IS NULL
         ORDER BY f.id",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    if files.is_empty() {
        return Ok(());
    }
    let root = std::env::current_dir()?;
    let parts = tokio::task::spawn_blocking(move || {
        let mut seen = HashSet::new();
        files
            .into_iter()
            .filter_map(|file| {
                let path = resolve(&root, &file)?;
                if !seen.insert(path.clone()) {
                    return None;
                }
                let index = index::refresh(&path, true).ok()?;
                if index.keyframes.is_empty() || index.duration_ms == 0 {
                    return None;
                }
                let metadata = std::fs::metadata(&path).ok()?;
                let relative = path
                    .strip_prefix(&root)
                    .ok()?
                    .to_string_lossy()
                    .into_owned();
                let container = if index.container == index::Container::Flv {
                    "flv"
                } else {
                    "ts"
                };
                let danmaku = path.with_extension("xml");
                let danmaku = danmaku
                    .is_file()
                    .then(|| danmaku.to_string_lossy().into_owned());
                Some((
                    relative,
                    container,
                    i64::from(index.duration_ms),
                    metadata.len() as i64,
                    danmaku,
                ))
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(std::io::Error::other)?;
    let mut tx = pool.begin().await?;
    let mut start = 0i64;
    for (path, container, length, bytes, danmaku) in parts {
        sqlx::query(
            "INSERT INTO segments (session_id, path, container, state, start_ms, end_ms, bytes, danmaku_path)
             SELECT id, ?, ?, 'finished', ?, ?, ?, ? FROM stream_sessions
             WHERE id = ? AND deleted_at IS NULL AND started_at IS NULL",
        )
        .bind(path).bind(container).bind(start).bind(start + length).bind(bytes).bind(danmaku).bind(id)
        .execute(&mut *tx).await?;
        start += length;
    }
    tx.commit().await?;
    Ok(())
}
