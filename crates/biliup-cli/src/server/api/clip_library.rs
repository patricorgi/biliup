use crate::server::infrastructure::connection_pool::ConnectionPool;
use crate::server::workbench::clips::edits;
use crate::server::workbench::clips::export::ClipExports;
use crate::server::workbench::clips::hardsubs;
use crate::server::workbench::clips::imports::{self, ImportSummary, Imports};
use crate::server::workbench::clips::library::{
    self, Container, Input, Job, Kind, Range, Selection, StartError, Status,
};
use crate::server::workbench::clips::publish::{cover_file, queue::ClipPublisher};
use crate::server::workbench::clips::subtitles;
use crate::server::workbench::clips::{self, Clip, State as ClipState};
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tower::ServiceExt;
use tower_http::services::ServeFile;

type ApiError = (StatusCode, String);
const MISSING: &str = "找不到已导出的切片";

#[derive(Serialize)]
struct SavedClip {
    #[serde(flatten)]
    clip: super::clips::ClipView,
    session_title: String,
}

#[derive(Serialize)]
pub struct Library {
    clips: Vec<SavedClip>,
    imports: Vec<ImportedView>,
    jobs: Vec<Job>,
    edits: HashMap<i64, String>,
    edit_durations_ms: HashMap<i64, i64>,
    subtitles: HashMap<String, subtitles::State>,
    hardsubs: HashMap<String, hardsubs::State>,
}

#[derive(Serialize)]
struct ImportedView {
    id: i64,
    file_name: String,
    source_path: String,
    duration_ms: i64,
}

#[derive(Deserialize)]
pub struct ImportRequest {
    paths: Vec<String>,
}

pub async fn import(
    State(exports): State<Arc<ClipExports>>,
    Json(request): Json<ImportRequest>,
) -> Result<Json<ImportSummary>, ApiError> {
    if request.paths.is_empty() || request.paths.len() > 200 {
        return Err((StatusCode::BAD_REQUEST, "请输入 1 至 200 个路径".into()));
    }
    imports::add(&exports.library_dir(), &request.paths)
        .await
        .map(Json)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidInput {
                (StatusCode::BAD_REQUEST, error.to_string())
            } else {
                internal(error)
            }
        })
}

pub async fn list(
    State(pool): State<ConnectionPool>,
    State(exports): State<Arc<ClipExports>>,
) -> Result<Json<Library>, ApiError> {
    let saved = clips::list_saved(&pool).await.map_err(internal)?;
    let mut items = Vec::new();
    for (clip, session_title) in saved {
        if saved_path(&clip, &exports).await.is_some() {
            items.push(SavedClip {
                clip: super::clips::view(clip, &exports),
                session_title,
            });
        }
    }
    let root = exports.library_dir();
    let imported = Imports::load(&root).await.map_err(internal)?;
    let mut imported_views = Vec::new();
    for item in imported.items.iter().rev() {
        if imported.path(item).await.is_some() {
            imported_views.push(ImportedView {
                id: item.id,
                file_name: item.filename(),
                source_path: item.display_path(),
                duration_ms: item.duration_ms,
            });
        }
    }
    let jobs = library::list(&root).await.map_err(internal)?;
    let current = edits::current(&root, &jobs).await.map_err(internal)?;
    let mut active_edits = HashMap::new();
    let mut edit_durations_ms = HashMap::new();
    for (id, job) in current {
        active_edits.insert(id, job.id.clone());
        if let Some(selection) = &job.selection {
            edit_durations_ms.insert(id, selection.range.end_ms - selection.range.start_ms);
        } else if let Some(path) = managed_path(&root, job).await
            && let Some(duration) = edits::duration_ms(path).await
        {
            edit_durations_ms.insert(id, duration);
        }
    }
    let mut subtitle_states = HashMap::new();
    let mut hardsub_states = HashMap::new();
    if let Ok(root) = fs::canonicalize(&root).await {
        for job in &jobs {
            if job.kind == Kind::Join && job.status == Status::Ready {
                let video = root.join(job.filename());
                if let Some(state) = subtitles::status(&video.with_extension("srt")).await {
                    subtitle_states.insert(job.id.clone(), state);
                }
                if let Some(state) = hardsubs::status(&video) {
                    hardsub_states.insert(job.id.clone(), state);
                }
            }
        }
    }
    Ok(Json(Library {
        clips: items,
        imports: imported_views,
        jobs,
        edits: active_edits,
        edit_durations_ms,
        subtitles: subtitle_states,
        hardsubs: hardsub_states,
    }))
}

pub async fn trim(
    State(pool): State<ConnectionPool>,
    State(exports): State<Arc<ClipExports>>,
    Path(id): Path<i64>,
    Json(range): Json<Range>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    if !range.valid() {
        return Err((StatusCode::BAD_REQUEST, "请输入有效的切片入点和出点".into()));
    }
    let _guard = edits::MUTATION.lock().await;
    let root = exports.library_dir();
    let jobs = library::list(&root).await.map_err(internal)?;
    if trimming(&jobs, id) {
        return Err((StatusCode::CONFLICT, "这段切片正在精剪，请稍后再试".into()));
    }
    let current = edits::current(&root, &jobs).await.map_err(internal)?;
    let previous = current.get(&id).copied();
    let input = selected_input(&pool, &exports, id, previous).await?;
    let (offset, limit, base_edit) = match previous {
        Some(job) => match &job.selection {
            Some(selection) => (
                selection.range.start_ms,
                Some(selection.range.end_ms),
                selection.base_edit.clone(),
            ),
            None => (
                0,
                edits::duration_ms(input.path).await,
                Some(job.id.clone()),
            ),
        },
        None => (0, None, None),
    };
    let range = Range {
        start_ms: offset.saturating_add(range.start_ms),
        end_ms: offset.saturating_add(range.end_ms),
    };
    if limit.is_some_and(|end| range.end_ms > end) {
        return Err((StatusCode::BAD_REQUEST, "出点超出当前切片范围".into()));
    }
    let latest = jobs
        .iter()
        .filter(|job| job.kind == Kind::Trim && job.clip_ids == [id])
        .map(|job| job.created_at)
        .max();
    let job = library::record_range(&root, id, Selection { range, base_edit }, latest)
        .await
        .map_err(internal)?;
    Ok((StatusCode::CREATED, Json(job)))
}

pub async fn restore_trim(
    State(pool): State<ConnectionPool>,
    State(exports): State<Arc<ClipExports>>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let _guard = edits::MUTATION.lock().await;
    source_path(&pool, &exports, id).await?;
    let root = exports.library_dir();
    let jobs = library::list(&root).await.map_err(internal)?;
    if trimming(&jobs, id) {
        return Err((StatusCode::CONFLICT, "这段切片正在精剪，请稍后再试".into()));
    }
    if edits::current(&root, &jobs)
        .await
        .map_err(internal)?
        .contains_key(&id)
    {
        edits::restore(&root, id, &jobs).await.map_err(internal)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

fn trimming(jobs: &[Job], id: i64) -> bool {
    jobs.iter().any(|job| {
        job.kind == Kind::Trim
            && job.clip_ids == [id]
            && matches!(job.status, Status::Queued | Status::Running)
    })
}

#[derive(Deserialize)]
pub struct JoinRequest {
    items: Vec<JoinItem>,
    #[serde(default)]
    source_format: bool,
    #[serde(default)]
    fade_out: bool,
}

#[derive(Deserialize)]
struct JoinItem {
    clip_id: i64,
    edit_id: Option<String>,
}

pub async fn join(
    State(pool): State<ConnectionPool>,
    State(exports): State<Arc<ClipExports>>,
    Json(request): Json<JoinRequest>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let ids: Vec<_> = request.items.iter().map(|item| item.clip_id).collect();
    if !(2..=library::MAX_JOIN_CLIPS).contains(&ids.len())
        || ids.iter().collect::<HashSet<_>>().len() != ids.len()
    {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("请选择 2 至 {} 个不同的切片", library::MAX_JOIN_CLIPS),
        ));
    }
    let _guard = edits::MUTATION.lock().await;
    let mut inputs = Vec::new();
    let root = exports.library_dir();
    let jobs = library::list(&root).await.map_err(internal)?;
    let current = edits::current(&root, &jobs).await.map_err(internal)?;
    let mut format = None;
    for item in request.items {
        if request.source_format && !request.fade_out {
            let original = source_path(&pool, &exports, item.clip_id).await?;
            let container = Container::from_path(&original).ok_or((
                StatusCode::BAD_REQUEST,
                "这类源文件格式暂不支持直接拼接".into(),
            ))?;
            if format.is_some_and(|first| first != container) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "请选用同一种源文件格式进行拼接".into(),
                ));
            }
            format = Some(container);
        }
        let explicit = match item.edit_id {
            Some(edit_id) => Some(
                library::get(&root, &edit_id)
                    .await
                    .filter(|job| {
                        job.status == Status::Ready
                            && job.kind == Kind::Trim
                            && job.clip_ids == [item.clip_id]
                    })
                    .ok_or_else(not_found)?,
            ),
            None => None,
        };
        let selected = explicit
            .as_ref()
            .or_else(|| current.get(&item.clip_id).copied());
        inputs.push(selected_input(&pool, &exports, item.clip_id, selected).await?);
    }
    start(&exports, ids, inputs, format.unwrap_or_default(), request.fade_out).await
}

pub async fn preview(
    State(pool): State<ConnectionPool>,
    State(exports): State<Arc<ClipExports>>,
    Path(id): Path<i64>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let root = exports.library_dir();
    let jobs = library::list(&root).await.map_err(internal)?;
    if let Some(job) = edits::current(&root, &jobs)
        .await
        .map_err(internal)?
        .get(&id)
    {
        let input = selected_input(&pool, &exports, id, Some(job)).await?;
        let key = match &job.selection {
            Some(selection) => selection
                .base_edit
                .clone()
                .unwrap_or_else(|| format!("range-base-{id}")),
            None => job.id.clone(),
        };
        return serve_preview(&root, input.path, &key, request).await;
    }
    if id < 0 {
        let path = imported_path(&exports, id).await?;
        return serve_preview(&root, path, &format!("imported{id}"), request).await;
    }
    let clip = find_clip(&pool, id).await?;
    saved_path(&clip, &exports).await.ok_or_else(not_found)?;
    let path = exports
        .mp4(&clip)
        .await
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
    serve(path, None, request).await
}

pub async fn imported_download(
    State(exports): State<Arc<ClipExports>>,
    Path(id): Path<i64>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let path = imported_path(&exports, id).await?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    serve(path, name, request).await
}

async fn imported_path(exports: &ClipExports, id: i64) -> Result<PathBuf, ApiError> {
    let library = Imports::load(&exports.library_dir())
        .await
        .map_err(internal)?;
    let item = library
        .items
        .iter()
        .find(|item| item.id == id)
        .ok_or_else(not_found)?;
    library.path(item).await.ok_or_else(not_found)
}

async fn source_path(
    pool: &ConnectionPool,
    exports: &ClipExports,
    id: i64,
) -> Result<PathBuf, ApiError> {
    if id < 0 {
        imported_path(exports, id).await
    } else {
        let clip = find_clip(pool, id).await?;
        saved_path(&clip, exports).await.ok_or_else(not_found)
    }
}

async fn selected_input(
    pool: &ConnectionPool,
    exports: &ClipExports,
    id: i64,
    edit: Option<&Job>,
) -> Result<Input, ApiError> {
    let root = exports.library_dir();
    if let Some(job) = edit {
        if let Some(selection) = &job.selection {
            let path = if let Some(base) = &selection.base_edit {
                let job = library::get(&root, base)
                    .await
                    .filter(|job| {
                        job.kind == Kind::Trim
                            && job.status == Status::Ready
                            && job.clip_ids == [id]
                            && job.selection.is_none()
                    })
                    .ok_or_else(not_found)?;
                managed_path(&root, &job).await.ok_or_else(not_found)?
            } else {
                source_path(pool, exports, id).await?
            };
            return Ok(Input {
                path,
                range: Some(selection.range),
            });
        }
        let path = managed_path(&root, job).await.ok_or_else(not_found)?;
        return Ok(Input { path, range: None });
    }
    Ok(Input {
        path: source_path(pool, exports, id).await?,
        range: None,
    })
}

#[derive(Deserialize)]
pub struct DownloadQuery {
    #[serde(default)]
    download: bool,
    #[serde(default)]
    mp4: bool,
}

pub async fn output(
    State(exports): State<Arc<ClipExports>>,
    Path(id): Path<String>,
    Query(query): Query<DownloadQuery>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let root = exports.library_dir();
    let job = library::get(&root, &id)
        .await
        .filter(|job| job.status == Status::Ready)
        .ok_or_else(not_found)?;
    let path = managed_path(&root, &job).await.ok_or_else(not_found)?;
    if query.mp4 || !query.download {
        let mp4 = imports::preview(&root, &path, &job.id)
            .await
            .map_err(|error| (StatusCode::CONFLICT, error))?;
        let filename = PathBuf::from(job.filename()).with_extension("mp4");
        return serve(
            mp4,
            query
                .download
                .then(|| filename.to_string_lossy().into_owned()),
            request,
        )
        .await;
    }
    serve(path, query.download.then(|| job.filename()), request).await
}

async fn serve_preview(
    root: &FsPath,
    path: PathBuf,
    key: &str,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let path = imports::preview(root, &path, key)
        .await
        .map_err(|error| (StatusCode::CONFLICT, error))?;
    serve(path, None, request).await
}

pub async fn generate_subtitles(
    State(exports): State<Arc<ClipExports>>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<subtitles::State>), ApiError> {
    let _guard = subtitles::EDIT_LOCK.lock().await;
    let video = joined_output(&exports, &id).await?;
    let job_dir = exports.library_dir().join(".jobs").join(id);
    let state = subtitles::start(video, job_dir)
        .await
        .map_err(|error| match error {
            subtitles::StartError::Busy => (
                StatusCode::TOO_MANY_REQUESTS,
                "正在为另一段成片生成字幕，请稍后重试".into(),
            ),
            subtitles::StartError::MissingModel => (
                StatusCode::SERVICE_UNAVAILABLE,
                "服务器未配置中文语音识别模型".into(),
            ),
        })?;
    Ok((StatusCode::ACCEPTED, Json(state)))
}

#[derive(Deserialize)]
pub struct TrashClipsRequest {
    ids: Vec<i64>,
}

#[derive(Serialize, Default)]
pub struct TrashClipsResult {
    removed: Vec<i64>,
    errors: Vec<String>,
}

pub async fn trash_clips(
    State(pool): State<ConnectionPool>,
    State(exports): State<Arc<ClipExports>>,
    State(publisher): State<Arc<ClipPublisher>>,
    Json(request): Json<TrashClipsRequest>,
) -> Result<Json<TrashClipsResult>, ApiError> {
    if request.ids.is_empty()
        || request.ids.len() > library::MAX_JOIN_CLIPS
        || request.ids.contains(&0)
        || request.ids.iter().collect::<HashSet<_>>().len() != request.ids.len()
    {
        return Err((StatusCode::BAD_REQUEST, "请选择有效的切片".into()));
    }
    tokio::spawn(async move {
        let _edits = edits::MUTATION.lock().await;
        let _imports = imports::WRITING.lock().await;
        let _preview = imports::REMUX.lock().await;
        let root = exports.library_dir();
        fs::create_dir_all(&root).await.map_err(internal)?;
        let root = fs::canonicalize(root).await.map_err(internal)?;
        let mut imported = Imports::load(&root).await.map_err(internal)?;
        let jobs = library::list(&root).await.map_err(internal)?;
        let mut result = TrashClipsResult::default();
        for id in request.ids {
            let name = if id < 0 {
                imported
                    .items
                    .iter()
                    .find(|clip| clip.id == id)
                    .map(|clip| clip.filename())
            } else {
                None
            }
            .unwrap_or_else(|| format!("切片 #{id}"));
            match trash_one_clip(&pool, &exports, &publisher, &root, &mut imported, &jobs, id).await
            {
                Ok(()) => result.removed.push(id),
                Err(message) => result.errors.push(format!("{name}：{message}")),
            }
        }
        Ok(Json(result))
    })
    .await
    .map_err(internal)?
}

async fn trash_one_clip(
    pool: &ConnectionPool,
    exports: &ClipExports,
    publisher: &ClipPublisher,
    root: &FsPath,
    imported: &mut Imports,
    jobs: &[Job],
    id: i64,
) -> Result<(), String> {
    if trimming(jobs, id)
        || jobs
            .iter()
            .any(|job| job.kind == Kind::Trim && job.clip_ids == [id] && job.progress.is_some())
        || exports.progress(id).is_some()
        || publisher.state_of(id).is_some()
    {
        return Err("切片正在处理或处于发布队列中，请稍后再试".into());
    }
    let _remux = if id > 0 {
        Some(exports.lock_remux(id).await)
    } else {
        None
    };
    let (source, session) = if id < 0 {
        (
            imported
                .trash_path(id)
                .await
                .map_err(|_| "找不到原始导入文件，或其路径已变化")?,
            None,
        )
    } else {
        let clip = find_clip(pool, id).await.map_err(|(_, message)| message)?;
        let source = saved_path(&clip, exports)
            .await
            .ok_or("找不到已导出的切片")?;
        let stored = FsPath::new(clip.output_path.as_deref().ok_or("找不到已导出的切片")?);
        if !fs::symlink_metadata(stored)
            .await
            .is_ok_and(|meta| meta.is_file())
            || source.file_stem().and_then(|name| name.to_str()) != Some(id.to_string().as_str())
        {
            return Err("切片文件路径已变化，无法安全删除".into());
        }
        (source, Some(clip.session_id))
    };
    let trims: Vec<_> = jobs
        .iter()
        .filter(|job| job.kind == Kind::Trim && job.clip_ids == [id])
        .cloned()
        .collect();
    let root_owned = root.to_path_buf();
    tokio::task::spawn_blocking(move || trash_clip_files(&root_owned, source, session, id, &trims))
        .await
        .map_err(|error| internal(error).1)?
        .map_err(|error| {
            tracing::warn!(%error, id, "无法将切片移入废纸篓");
            "无法移入废纸篓，请检查文件或磁盘权限".to_owned()
        })?;
    let metadata_result = if let Some(session) = session {
        clips::delete(pool, session, id)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    } else {
        imported
            .mark_deleted(root, id)
            .await
            .map_err(|error| error.to_string())
    };
    if let Err(error) = metadata_result {
        tracing::warn!(%error, id, "切片已移入废纸篓，但无法清理切片记录");
        return Err("文件已移入废纸篓，但切片记录清理失败".into());
    }
    if let Err(error) = edits::forget(root, id).await {
        tracing::warn!(%error, id, "无法清理已删除切片的还原记录");
    }
    Ok(())
}

fn trash_clip_files(
    root: &FsPath,
    source: PathBuf,
    session: Option<i64>,
    id: i64,
    trims: &[Job],
) -> std::io::Result<()> {
    use std::fs;
    use std::io::{Error, ErrorKind};

    let mut paths = vec![(
        source.clone(),
        PathBuf::from("原切片").join(
            source
                .file_name()
                .ok_or_else(|| Error::other("invalid clip name"))?,
        ),
    )];
    if session.is_some() {
        let directory = source
            .parent()
            .ok_or_else(|| Error::other("invalid clip directory"))?;
        let prefix = format!("{id}.");
        let cover = cover_file(directory, id);
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            if path != source
                && (entry.file_name().to_string_lossy().starts_with(&prefix) || path == cover)
            {
                if !entry.file_type()?.is_file() {
                    return Err(Error::other("invalid clip output"));
                }
                paths.push((path, PathBuf::from("原切片").join(entry.file_name())));
            }
        }
    }
    let jobs = root.join(".jobs");
    let previews = root.join(".previews");
    for directory in [&jobs, &previews] {
        match fs::symlink_metadata(directory) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(Error::other("invalid managed directory")),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let mut preview_keys = vec![format!("imported{id}"), format!("range-base-{id}")];
    for job in trims {
        let metadata = jobs.join(&job.id);
        if !fs::symlink_metadata(&metadata)?.is_dir() {
            return Err(Error::other("invalid trim directory"));
        }
        paths.push((metadata, PathBuf::from("精剪记录").join(&job.id)));
        let name = job.filename();
        let path = root.join(&name);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => paths.push((path, PathBuf::from("精剪文件").join(name))),
            Ok(_) => return Err(Error::other("invalid trim file")),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        preview_keys.push(job.id.clone());
    }
    for key in preview_keys {
        let name = format!("{key}.mp4");
        let path = previews.join(&name);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => paths.push((path, PathBuf::from("预览").join(name))),
            Ok(_) => return Err(Error::other("invalid preview file")),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    trash_bundle(root, &format!("切片-{id}-"), paths)
}

/// Each bundle is moved to the system Trash as one item. Roll back staging if
/// Trash fails. Files imported from another volume are first moved aside on
/// their own volume, then copied into the bundle.
fn trash_bundle(
    root: &FsPath,
    prefix: &str,
    paths: Vec<(PathBuf, PathBuf)>,
) -> std::io::Result<()> {
    use std::fs;
    use std::io::{Error, ErrorKind};

    let bundle = tempfile::Builder::new().prefix(prefix).tempdir_in(root)?;
    let mut staged = Vec::new();
    let result = (|| {
        for (path, relative) in paths {
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.is_file() && !metadata.is_dir() {
                return Err(Error::other("unexpected clip file"));
            }
            let target = bundle.path().join(relative);
            fs::create_dir_all(target.parent().expect("path in trash bundle"))?;
            match fs::rename(&path, &target) {
                Ok(()) => staged.push((path, target, None)),
                Err(error) if error.kind() == ErrorKind::CrossesDevices && metadata.is_file() => {
                    let temporary = tempfile::Builder::new()
                        .prefix(".biliup-trash-")
                        .tempdir_in(path.parent().expect("clip has a parent"))?;
                    let held = temporary.path().join("original");
                    fs::rename(&path, &held)?;
                    staged.push((path, held.clone(), Some(temporary)));
                    if fs::copy(&held, &target)? != metadata.len() {
                        return Err(Error::other("clip changed while being copied"));
                    }
                }
                Err(error) => return Err(error),
            }
        }
        super::recording_trash::send_to_trash(bundle.path()).map_err(Error::other)
    })();
    if result.is_err() {
        let mut keep_bundle = false;
        for (original, moved, temporary) in staged.into_iter().rev() {
            let restore = match fs::symlink_metadata(&original) {
                Err(error) if error.kind() == ErrorKind::NotFound => fs::rename(moved, original),
                Err(error) => Err(error),
                Ok(_) => Err(Error::new(
                    ErrorKind::AlreadyExists,
                    "original path was reused",
                )),
            };
            if let Err(error) = restore {
                tracing::warn!(%error, "无法撤回移入废纸篓的文件");
                if let Some(temporary) = temporary {
                    let _ = temporary.keep();
                } else {
                    keep_bundle = true;
                }
            }
        }
        if keep_bundle {
            let _ = bundle.keep();
        }
    }
    result
}

pub async fn trash_join(
    State(exports): State<Arc<ClipExports>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let root = exports.library_dir();
    tokio::spawn(async move {
        let _guard = subtitles::EDIT_LOCK.lock().await;
        let _preview = imports::REMUX.lock().await;
        let job = library::get(&root, &id)
            .await
            .filter(|job| job.kind == Kind::Join)
            .ok_or_else(not_found)?;
        let root = fs::canonicalize(root).await.map_err(internal)?;
        let video = root.join(job.filename());
        if matches!(job.status, Status::Queued | Status::Running)
            || job.progress.is_some()
            || hardsubs::in_progress(&video)
            || matches!(
                subtitles::status(&video.with_extension("srt")).await,
                Some(subtitles::State::Running)
            )
        {
            return Err((
                StatusCode::CONFLICT,
                "成片或字幕仍在处理中，请稍后再试".into(),
            ));
        }
        tokio::task::spawn_blocking(move || trash_join_files(&root, &job))
            .await
            .map_err(internal)?
            .map_err(|error| {
                tracing::warn!(%error, "拼接成片未能移入废纸篓");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "无法将拼接结果移入废纸篓，请检查磁盘和权限".into(),
                )
            })?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(internal)?
}

fn trash_join_files(root: &FsPath, job: &Job) -> std::io::Result<()> {
    use std::fs;
    use std::io::{Error, ErrorKind};

    let jobs = root.join(".jobs");
    let previews = root.join(".previews");
    let metadata = jobs.join(&job.id);
    if !fs::symlink_metadata(&jobs)?.is_dir() || !fs::symlink_metadata(&metadata)?.is_dir() {
        return Err(Error::other("invalid join directory"));
    }
    let video = root.join(job.filename());
    let mut files = vec![
        video.clone(),
        video.with_extension("srt"),
        hardsubs::path(&video),
    ];
    match fs::symlink_metadata(&previews) {
        Ok(meta) if meta.is_dir() => files.push(previews.join(format!("{}.mp4", job.id))),
        Ok(_) => return Err(Error::other("invalid preview directory")),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut present = Vec::new();
    for path in files {
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => present.push(path),
            Ok(_) => return Err(Error::other("invalid join file")),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let paths = present
        .into_iter()
        .map(|path| {
            let name = path.file_name().expect("managed join file").into();
            (path, name)
        })
        .chain(std::iter::once((metadata, PathBuf::from("任务记录"))))
        .collect();
    trash_bundle(root, &format!("拼接-{}-", job.id), paths)
}

#[derive(Deserialize)]
pub struct SubtitleQuery {
    #[serde(default)]
    vtt: bool,
}

pub async fn download_subtitles(
    State(exports): State<Arc<ClipExports>>,
    Path(id): Path<String>,
    Query(query): Query<SubtitleQuery>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let path = joined_output(&exports, &id).await?.with_extension("srt");
    if !subtitles::exists(&path).await {
        return Err((StatusCode::NOT_FOUND, "找不到成片字幕".into()));
    }
    if query.vtt {
        let srt = fs::read_to_string(&path).await.map_err(internal)?;
        return Ok((
            [(header::CONTENT_TYPE, "text/vtt; charset=utf-8")],
            subtitles::webvtt(&srt),
        )
            .into_response());
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let mut response = serve(path, name, request).await?;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-subrip; charset=utf-8"),
    );
    Ok(response)
}

pub async fn subtitle_draft(
    State(exports): State<Arc<ClipExports>>,
    Path(id): Path<String>,
) -> Result<Json<subtitles::Draft>, ApiError> {
    let video = joined_output(&exports, &id).await?;
    subtitles::draft(&video).await.map(Json).map_err(internal)
}

#[derive(Deserialize)]
pub struct SubtitleEdit {
    text: String,
    revision: String,
}

pub async fn edit_subtitles(
    State(exports): State<Arc<ClipExports>>,
    Path(id): Path<String>,
    Json(edit): Json<SubtitleEdit>,
) -> Result<Json<subtitles::Draft>, ApiError> {
    let _guard = subtitles::EDIT_LOCK.lock().await;
    let video = joined_output(&exports, &id).await?;
    if hardsubs::in_progress(&video) {
        return Err((
            StatusCode::CONFLICT,
            "内嵌字幕正在生成，请先等待完成或取消任务".into(),
        ));
    }
    let job_dir = exports.library_dir().join(".jobs").join(id);
    subtitles::edit(&video, &job_dir, &edit.text, &edit.revision)
        .await
        .map(Json)
        .map_err(|error| match error {
            subtitles::EditError::Conflict => (
                StatusCode::CONFLICT,
                "字幕已被修改，请重新打开后再编辑".into(),
            ),
            subtitles::EditError::Text(message) => (StatusCode::UNPROCESSABLE_ENTITY, message),
        })
}

pub(super) async fn joined_output(exports: &ClipExports, id: &str) -> Result<PathBuf, ApiError> {
    let root = exports.library_dir();
    let job = library::get(&root, id)
        .await
        .filter(|job| job.status == Status::Ready && job.kind == Kind::Join)
        .ok_or_else(not_found)?;
    managed_path(&root, &job).await.ok_or_else(not_found)
}

pub async fn generate_hardsubs(
    State(exports): State<Arc<ClipExports>>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<hardsubs::State>), ApiError> {
    let _guard = subtitles::EDIT_LOCK.lock().await;
    let video = joined_output(&exports, &id).await?;
    let job_dir = exports.library_dir().join(".jobs").join(id);
    let state = hardsubs::start(video, job_dir)
        .await
        .map_err(|error| match error {
            hardsubs::StartError::Busy => (
                StatusCode::TOO_MANY_REQUESTS,
                "正在为另一段成片生成内嵌字幕，请稍后重试".into(),
            ),
            hardsubs::StartError::MissingSrt => {
                (StatusCode::CONFLICT, "请先为这段成片生成字幕".into())
            }
            hardsubs::StartError::MissingFont => (
                StatusCode::SERVICE_UNAVAILABLE,
                "服务器未配置得意黑字体".into(),
            ),
        })?;
    Ok((StatusCode::ACCEPTED, Json(state)))
}

pub(super) async fn joined_hardsubs(exports: &ClipExports, id: &str) -> Result<PathBuf, ApiError> {
    let video = joined_output(exports, id).await?;
    hardsubs::current_path(&video)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "找不到带字幕的成片".into()))
}

#[derive(Deserialize)]
pub struct HardsubsQuery {
    #[serde(default)]
    inline: bool,
}

pub async fn download_hardsubs(
    State(exports): State<Arc<ClipExports>>,
    Path(id): Path<String>,
    Query(query): Query<HardsubsQuery>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let path = joined_hardsubs(&exports, &id).await?;
    if query.inline {
        let mut response = serve(path, None, request).await?;
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        return Ok(response);
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    serve(path, name, request).await
}

pub async fn cancel_hardsubs(
    State(exports): State<Arc<ClipExports>>,
    Path(id): Path<String>,
) -> Result<Json<hardsubs::State>, ApiError> {
    let video = joined_output(&exports, &id).await?;
    hardsubs::cancel(&video)
        .map(Json)
        .ok_or((StatusCode::CONFLICT, "没有可取消的内嵌字幕任务".into()))
}

async fn serve(
    path: PathBuf,
    download: Option<String>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let mut response = ServeFile::new(path)
        .oneshot(request)
        .await
        .map_err(internal)?
        .map(Body::new);
    if let Some(name) = download {
        let value = format!(
            "attachment; filename*=UTF-8''{}",
            urlencoding::encode(&name)
        );
        response.headers_mut().insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&value).map_err(internal)?,
        );
    }
    Ok(response)
}

async fn start(
    exports: &ClipExports,
    ids: Vec<i64>,
    inputs: Vec<Input>,
    format: Container,
    fade_out: bool,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let job = library::start(exports.library_dir(), ids, inputs, format, fade_out)
        .await
        .map_err(|error| match error {
            StartError::Busy => (
                StatusCode::TOO_MANY_REQUESTS,
                "切片处理队列已满，请稍后重试".into(),
            ),
            StartError::Storage(error) => internal(error),
        })?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn find_clip(pool: &ConnectionPool, id: i64) -> Result<Clip, ApiError> {
    clips::get(pool, id)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)
}

async fn saved_path(clip: &Clip, exports: &ClipExports) -> Option<PathBuf> {
    if !matches!(clip.state, ClipState::Ready | ClipState::Published) {
        return None;
    }
    let path = FsPath::new(clip.output_path.as_deref()?);
    let parent = fs::canonicalize(exports.dir(clip.session_id)).await.ok()?;
    let resolved = fs::canonicalize(path).await.ok()?;
    (resolved.parent() == Some(parent.as_path()) && fs::metadata(&resolved).await.ok()?.is_file())
        .then_some(resolved)
}

async fn managed_path(root: &FsPath, job: &Job) -> Option<PathBuf> {
    let root = fs::canonicalize(root).await.ok()?;
    let path = root.join(job.filename());
    fs::symlink_metadata(&path)
        .await
        .ok()?
        .is_file()
        .then_some(path)
}

fn not_found() -> ApiError {
    (StatusCode::NOT_FOUND, MISSING.into())
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    tracing::warn!(%error, "切片管理操作失败");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "无法读取或保存切片，请检查服务器日志和磁盘空间".into(),
    )
}
