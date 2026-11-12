use crate::server::infrastructure::connection_pool::ConnectionPool;
use crate::server::workbench::{clips::subtitles, legacy, live, store};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

type ApiError = (StatusCode, String);
type Cue = subtitles::TranscriptCue;
static JOBS: LazyLock<Mutex<HashMap<i64, Transcript>>> = LazyLock::new(Mutex::default);

#[derive(Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Transcript {
    Idle,
    Running { completed: usize, total: usize },
    Ready { cues: Vec<Cue> },
    Failed { error: String },
}

fn storage(error: impl std::fmt::Display) -> ApiError {
    tracing::warn!(%error, "无法读取或保存台词列表");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "无法读取或保存台词列表".into(),
    )
}

async fn saved(pool: &ConnectionPool, id: i64) -> Result<Option<Transcript>, ApiError> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT t.cues FROM session_transcripts t JOIN stream_sessions s ON s.id = t.session_id
         WHERE t.session_id = ? AND s.deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(storage)?;
    text.map(|text| {
        serde_json::from_str(&text)
            .map(|cues| Transcript::Ready { cues })
            .map_err(storage)
    })
    .transpose()
}

pub(crate) async fn get(
    State(pool): State<ConnectionPool>,
    Path(id): Path<i64>,
) -> Result<Json<Transcript>, ApiError> {
    let current = JOBS.lock().unwrap().get(&id).cloned();
    Ok(Json(match current {
        Some(state) => state,
        None => saved(&pool, id).await?.unwrap_or(Transcript::Idle),
    }))
}

pub(crate) async fn start(
    State(pool): State<ConnectionPool>,
    Path(id): Path<i64>,
) -> Result<Json<Transcript>, ApiError> {
    legacy::prepare(&pool, id).await.map_err(storage)?;
    let session = store::session_summary(&pool, id)
        .await
        .map_err(storage)?
        .ok_or((StatusCode::NOT_FOUND, "找不到可用的录像".into()))?;
    if live::is_recording(id) || session.ended_at.is_none() {
        return Err((StatusCode::CONFLICT, "请在录制结束后生成台词列表".into()));
    }
    if let Some(result) = saved(&pool, id).await? {
        return Ok(Json(result));
    }
    let parts: Vec<_> = store::session_segments(&pool, id)
        .await
        .map_err(storage)?
        .into_iter()
        .filter_map(|part| {
            let length = part.end_ms? - part.start_ms;
            (matches!(
                part.state,
                store::SegmentState::Finished | store::SegmentState::PendingDelete
            ) && length > 0
                && std::path::Path::new(&part.path).is_file())
            .then_some((part.path, part.start_ms, length))
        })
        .collect();
    if parts.is_empty() {
        return Err((StatusCode::NOT_FOUND, "找不到可识别的录像".into()));
    }
    let total = parts.len();
    let mut jobs = JOBS.lock().unwrap();
    if let Some(current @ Transcript::Running { .. }) = jobs.get(&id) {
        return Ok(Json(current.clone()));
    }
    if jobs
        .values()
        .any(|job| matches!(job, Transcript::Running { .. }))
    {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "正在为另一场录像识别台词，请稍后重试".into(),
        ));
    }
    let state = Transcript::Running {
        completed: 0,
        total,
    };
    jobs.insert(id, state.clone());
    tokio::spawn(async move {
        let result = async {
            let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
            let mut cues = Vec::new();
            for (index, (path, offset, length)) in parts.into_iter().enumerate() {
                let mut rows =
                    subtitles::transcript_cues(std::path::Path::new(&path), scratch.path(), length)
                        .await
                        .map_err(|error| format!("第 {} 段录像：{error}", index + 1))?;
                for row in &mut rows {
                    row.at_ms += offset;
                    if let Some(end) = &mut row.end_ms {
                        *end += offset;
                    }
                }
                cues.extend(rows);
                JOBS.lock().unwrap().insert(
                    id,
                    Transcript::Running {
                        completed: index + 1,
                        total,
                    },
                );
            }
            let text = serde_json::to_string(&cues).map_err(|e| e.to_string())?;
            sqlx::query(
                "INSERT OR IGNORE INTO session_transcripts (session_id, cues)
                         SELECT id, ? FROM stream_sessions WHERE id = ? AND deleted_at IS NULL",
            )
            .bind(text)
            .bind(id)
            .execute(&pool)
            .await
            .map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        }
        .await;
        let mut jobs = JOBS.lock().unwrap();
        if let Err(error) = result {
            jobs.insert(id, Transcript::Failed { error });
        } else {
            jobs.remove(&id);
        }
    });
    Ok(Json(state))
}
