use super::endpoints::video_modified_seconds;
use crate::server::infrastructure::connection_pool::ConnectionPool;
use crate::server::router::{StaticPathError, resolve_recording_path};
use crate::server::workbench::{self, store::SegmentState};
use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::{fs, sync::Mutex};
use tracing::{info, warn};

type TrashError = (StatusCode, &'static str);

// Serialize both entry points so two browser tabs cannot move the same recording concurrently.
static TRASH_LOCK: Mutex<()> = Mutex::const_new(());
const CHANGED: TrashError = (StatusCode::CONFLICT, "录像列表已变化，请重新确认");
const RECORDING: TrashError = (StatusCode::CONFLICT, "录像仍在录制，请稍后再试");

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecordingFile {
    name: String,
    update_time: u64,
    size: u64,
}

#[derive(Serialize)]
pub struct SessionRecordings {
    recording: bool,
    files: Vec<RecordingFile>,
}

#[derive(Deserialize)]
pub struct TrashSessionRequest {
    all: bool,
    files: Vec<RecordingFile>,
}

#[derive(Serialize)]
pub struct TrashSessionResult {
    moved: usize,
    error: Option<&'static str>,
    session_removed: bool,
}

struct PreparedRecording {
    path: PathBuf,
    segment_ids: Vec<i64>,
}

pub async fn get_session_recordings(
    State(pool): State<ConnectionPool>,
    AxumPath(id): AxumPath<i64>,
) -> Result<Json<SessionRecordings>, TrashError> {
    let root = recording_root()?;
    Ok(Json(session_recordings(&pool, &root, id).await?))
}

pub async fn trash_video(
    State(pool): State<ConnectionPool>,
    Json(video): Json<RecordingFile>,
) -> Result<StatusCode, TrashError> {
    let _lock = TRASH_LOCK.lock().await;
    let root = recording_root()?;
    let prepared = prepare_recording(&pool, &root, &video, None).await?;
    move_recording(&pool, &video, prepared).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn trash_session_recordings(
    State(pool): State<ConnectionPool>,
    AxumPath(id): AxumPath<i64>,
    Json(request): Json<TrashSessionRequest>,
) -> Result<Json<TrashSessionResult>, TrashError> {
    let _lock = TRASH_LOCK.lock().await;
    let root = recording_root()?;
    let current = session_recordings(&pool, &root, id).await?;
    if current.recording {
        return Err(RECORDING);
    }
    let available: BTreeMap<_, _> = current.files.iter().map(|f| (f.name.as_str(), f)).collect();
    let mut seen = HashSet::new();
    if (!request.all && request.files.is_empty())
        || (request.all && available.len() != request.files.len())
        || request
            .files
            .iter()
            .any(|f| !seen.insert(f.name.as_str()) || available.get(f.name.as_str()) != Some(&f))
    {
        return Err(CHANGED);
    }

    // Check every file before moving the first one. A system failure during the move can still
    // leave a partial result, so report exactly how many files reached the Trash.
    let mut prepared = Vec::with_capacity(request.files.len());
    for file in &request.files {
        prepared.push(prepare_recording(&pool, &root, file, Some(id)).await?);
    }
    let mut result = TrashSessionResult {
        moved: 0,
        error: None,
        session_removed: false,
    };
    for (file, prepared) in request.files.iter().zip(prepared) {
        if let Err((_, message)) = move_recording(&pool, file, prepared).await {
            result.error = Some(message);
            break;
        }
        result.moved += 1;
    }
    if request.all && result.error.is_none() {
        match remove_session_from_history(&pool, &root, id).await {
            Ok(()) => result.session_removed = true,
            Err((_, message)) => result.error = Some(message),
        }
    }
    Ok(Json(result))
}

async fn remove_session_from_history(
    pool: &ConnectionPool,
    root: &Path,
    id: i64,
) -> Result<(), TrashError> {
    let remaining = session_recordings(pool, root, id).await?;
    if remaining.recording {
        return Err(RECORDING);
    }
    if !remaining.files.is_empty() {
        return Err(CHANGED);
    }
    let result = sqlx::query(
        "UPDATE stream_sessions SET deleted_at = ?
         WHERE id = ? AND deleted_at IS NULL
           AND (started_at IS NULL OR ended_at IS NOT NULL)
           AND NOT EXISTS (
               SELECT 1 FROM segments WHERE session_id = ? AND state = 'recording'
           )",
    )
    .bind(workbench::recorder::now_ms())
    .bind(id)
    .bind(id)
    .execute(pool)
    .await
    .map_err(database_error)?;
    if result.rows_affected() != 1 {
        return Err(CHANGED);
    }
    Ok(())
}

fn recording_root() -> Result<PathBuf, TrashError> {
    std::env::current_dir().map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "无法读取录像目录"))
}

fn database_error(error: sqlx::Error) -> TrashError {
    warn!(%error, "查询录像记录失败");
    (StatusCode::INTERNAL_SERVER_ERROR, "无法核对录像记录")
}

async fn session_recordings(
    pool: &ConnectionPool,
    root: &Path,
    id: i64,
) -> Result<SessionRecordings, TrashError> {
    let (started_at, ended_at): (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT started_at, ended_at FROM stream_sessions WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(database_error)?
    .ok_or((StatusCode::NOT_FOUND, "直播场次不存在"))?;
    let segments = workbench::store::session_segments(pool, id)
        .await
        .map_err(database_error)?;
    let recording = workbench::live::is_recording(id)
        || (started_at.is_some() && ended_at.is_none())
        || segments
            .iter()
            .any(|segment| segment.state == SegmentState::Recording);
    let legacy_paths: Vec<String> =
        sqlx::query_scalar("SELECT file FROM filelist WHERE session_id = ?")
            .bind(id)
            .fetch_all(pool)
            .await
            .map_err(database_error)?;
    let canonical_root = root
        .canonicalize()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "无法读取录像目录"))?;
    let mut files = BTreeMap::new();
    // The segment path is updated by biliup's archive step. Older filelist entries
    // still identify recordings at their original paths.
    for stored in segments.iter().map(|s| &s.path).chain(&legacy_paths) {
        let absolute = workbench::segment_path(&root.join(stored));
        let Ok(relative) = absolute
            .strip_prefix(root)
            .or_else(|_| absolute.strip_prefix(&canonical_root))
        else {
            continue;
        };
        let Some(name) = relative.to_str() else {
            continue;
        };
        let name = name.replace(std::path::MAIN_SEPARATOR, "/");
        let Ok(path) = resolve_recording_path(root, &name) else {
            continue;
        };
        if let Ok(metadata) = fs::symlink_metadata(path).await
            && metadata.is_file()
        {
            files.insert(
                name.clone(),
                RecordingFile {
                    name,
                    size: metadata.len(),
                    update_time: video_modified_seconds(&metadata),
                },
            );
        }
    }
    Ok(SessionRecordings {
        recording,
        files: files.into_values().collect(),
    })
}

async fn prepare_recording(
    pool: &ConnectionPool,
    root: &Path,
    video: &RecordingFile,
    session: Option<i64>,
) -> Result<PreparedRecording, TrashError> {
    let path = resolve_recording_path(root, &video.name).map_err(|error| match error {
        StaticPathError::Invalid => (StatusCode::BAD_REQUEST, "无效的录制文件"),
        StaticPathError::NotFound => (StatusCode::NOT_FOUND, "录制文件已不存在，请刷新列表"),
    })?;
    let relative = workbench::segment_path(Path::new(&video.name));
    let absolute = root.join(&relative);
    let segments: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT id, state, session_id FROM segments WHERE path IN (?, ?, ?) AND state != 'deleted'",
    )
    .bind(relative.to_string_lossy().as_ref())
    .bind(absolute.to_string_lossy().as_ref())
    .bind(path.to_string_lossy().as_ref())
    .fetch_all(pool)
    .await
    .map_err(database_error)?;
    if segments.iter().any(|(_, state, _)| state == "recording")
        || [&relative, &absolute, &path]
            .iter()
            .any(|p| workbench::index::live::is_live(p))
    {
        return Err(RECORDING);
    }
    if let Some(id) = session {
        let belongs_elsewhere: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM filelist WHERE file IN (?, ?, ?, ?) AND session_id != ?)",
        )
        .bind(relative.to_string_lossy().as_ref())
        .bind(Path::new(".").join(&relative).to_string_lossy().as_ref())
        .bind(absolute.to_string_lossy().as_ref())
        .bind(path.to_string_lossy().as_ref())
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(database_error)?;
        if belongs_elsewhere || segments.iter().any(|(_, _, owner)| *owner != id) {
            return Err((StatusCode::CONFLICT, "录像路径同时关联到其他场次"));
        }
    }
    check_file(&path, video).await?;
    Ok(PreparedRecording {
        path,
        segment_ids: segments.into_iter().map(|(id, _, _)| id).collect(),
    })
}

async fn check_file(path: &Path, video: &RecordingFile) -> Result<(), TrashError> {
    let metadata = fs::symlink_metadata(path)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "录制文件已不存在，请刷新列表"))?;
    if !metadata.is_file()
        || metadata.len() != video.size
        || video_modified_seconds(&metadata) != video.update_time
    {
        return Err(CHANGED);
    }
    Ok(())
}

async fn move_recording(
    pool: &ConnectionPool,
    video: &RecordingFile,
    prepared: PreparedRecording,
) -> Result<(), TrashError> {
    check_file(&prepared.path, video).await?;
    let trash_path = prepared.path.clone();
    tokio::task::spawn_blocking(move || send_to_trash(trash_path))
        .await
        .map_err(|error| {
            warn!(%error, "移入废纸篓的任务失败");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "无法将录制文件移入废纸篓",
            )
        })?
        .map_err(|error| {
            warn!(%error, name = %video.name, "录制文件未能移入废纸篓");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "无法将录制文件移入废纸篓",
            )
        })?;

    for id in prepared.segment_ids {
        if let Err(error) =
            workbench::store::set_segment_state(pool, id, SegmentState::Deleted).await
        {
            warn!(%error, id, "录像已移入废纸篓，但未能更新分段状态");
        }
    }
    workbench::index::remove(Path::new(&video.name));
    workbench::index::remove(&prepared.path);
    info!(name = %video.name, "已将录制文件移入废纸篓");
    Ok(())
}

pub(super) fn send_to_trash(path: impl AsRef<Path>) -> Result<(), trash::Error> {
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut context = trash::TrashContext::default();
        context.set_delete_method(DeleteMethod::NsFileManager);
        context.delete(path)
    }
    #[cfg(not(target_os = "macos"))]
    trash::delete(path)
}
