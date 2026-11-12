use super::export::ffmpeg_message;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use tokio::{
    fs,
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
    sync::Semaphore,
};
use tracing::warn;

pub const MAX_JOIN_CLIPS: usize = 30;
static QUEUE: Semaphore = Semaphore::const_new(8);
pub(super) static ENCODER: Semaphore = Semaphore::const_new(1);
static STREAM_COPY: Semaphore = Semaphore::const_new(1);
static ACTIVE: LazyLock<Mutex<HashMap<String, Progress>>> = LazyLock::new(Mutex::default);

#[derive(Clone, Deserialize, Serialize)]
pub struct Progress {
    stage: String,
    ratio: Option<f64>,
}

fn progress(id: &str, stage: &str, ratio: Option<f64>) {
    if let Some(current) = ACTIVE.lock().unwrap().get_mut(id) {
        *current = Progress {
            stage: stage.into(),
            ratio,
        };
    }
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Trim,
    Join,
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Queued,
    Running,
    Ready,
    Failed,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Job {
    pub id: String,
    pub kind: Kind,
    pub status: Status,
    pub clip_ids: Vec<i64>,
    pub created_at: i64,
    pub error: Option<String>,
    #[serde(default)]
    pub progress: Option<Progress>,
    #[serde(default)]
    pub cut: Option<Range>,
    #[serde(default)]
    pub selection: Option<Selection>,
    #[serde(default)]
    pub format: Container,
    #[serde(default)]
    pub fade_out: bool,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Container {
    #[default]
    Mp4,
    Flv,
    M4v,
    Mov,
    Mkv,
    Ts,
    Webm,
}

impl Container {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "mp4" => Some(Self::Mp4),
            "flv" => Some(Self::Flv),
            "m4v" => Some(Self::M4v),
            "mov" => Some(Self::Mov),
            "mkv" => Some(Self::Mkv),
            "ts" => Some(Self::Ts),
            "webm" => Some(Self::Webm),
            _ => None,
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Flv => "flv",
            Self::M4v => "m4v",
            Self::Mov => "mov",
            Self::Mkv => "mkv",
            Self::Ts => "ts",
            Self::Webm => "webm",
        }
    }

    fn muxer(self) -> &'static str {
        match self {
            Self::M4v => "mp4",
            Self::Mkv => "matroska",
            Self::Ts => "mpegts",
            _ => self.extension(),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Selection {
    #[serde(flatten)]
    pub range: Range,
    pub base_edit: Option<String>,
}

impl Job {
    pub fn filename(&self) -> String {
        let kind = match self.kind {
            Kind::Trim => "精剪",
            Kind::Join => "拼接",
        };
        format!("{kind}-{}.{}", self.id, self.format.extension())
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct Range {
    pub start_ms: i64,
    pub end_ms: i64,
}

impl Range {
    pub fn valid(self) -> bool {
        self.start_ms >= 0
            && self.end_ms > self.start_ms
            && self.end_ms - self.start_ms <= super::MAX_CLIP_MS
    }
}

pub struct Input {
    pub path: PathBuf,
    pub range: Option<Range>,
}

pub enum StartError {
    Busy,
    Storage(io::Error),
}

fn jobs_dir(root: &Path) -> PathBuf {
    root.join(".jobs")
}

fn job_path(root: &Path, id: &str) -> PathBuf {
    jobs_dir(root).join(id).join("job.json")
}

fn valid_id(id: &str) -> bool {
    id.strip_prefix("job-").is_some_and(|suffix| {
        suffix.len() == 6 && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
    })
}

pub async fn get(root: &Path, id: &str) -> Option<Job> {
    if !valid_id(id) {
        return None;
    }
    let data = fs::read(job_path(root, id)).await.ok()?;
    let mut job: Job = serde_json::from_slice(&data).ok()?;
    if job.id != id {
        return None;
    }
    job.progress = ACTIVE.lock().unwrap().get(id).cloned();
    let active = job.progress.is_some();
    if !active && matches!(job.status, Status::Queued | Status::Running) {
        if let Ok(mut entries) = fs::read_dir(jobs_dir(root).join(id)).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                if entry.file_name().to_string_lossy().starts_with("work-")
                    && entry.file_type().await.is_ok_and(|kind| kind.is_dir())
                {
                    let _ = fs::remove_dir_all(entry.path()).await;
                }
            }
        }
        if output_exists(root, &job).await {
            job.status = Status::Ready;
        } else {
            job.status = Status::Failed;
            job.error = Some("任务因服务重启而中断，请重新提交".into());
        }
    }
    if job.status == Status::Ready && job.selection.is_none() && !output_exists(root, &job).await {
        job.status = Status::Failed;
        job.error = Some("成片文件已不存在".into());
    }
    Some(job)
}

async fn output_exists(root: &Path, job: &Job) -> bool {
    fs::symlink_metadata(root.join(job.filename()))
        .await
        .is_ok_and(|meta| meta.is_file())
}

pub async fn list(root: &Path) -> io::Result<Vec<Job>> {
    let mut entries = match fs::read_dir(jobs_dir(root)).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut jobs = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        if let Some(id) = entry.file_name().to_str()
            && let Some(job) = get(root, id).await
        {
            jobs.push(job);
        }
    }
    jobs.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(jobs)
}

async fn save(root: &Path, job: &Job) -> io::Result<()> {
    let path = job_path(root, &job.id);
    let part = path.with_extension("json.part");
    fs::write(&part, serde_json::to_vec(job)?).await?;
    fs::rename(&part, &path).await
}

pub async fn record_range(
    root: &Path,
    clip: i64,
    selection: Selection,
    latest: Option<i64>,
) -> io::Result<Job> {
    let folder = jobs_dir(root);
    fs::create_dir_all(&folder).await?;
    let entry = tempfile::Builder::new().prefix("job-").tempdir_in(folder)?;
    let id = entry
        .path()
        .file_name()
        .expect("job folder")
        .to_string_lossy()
        .into_owned();
    let now = crate::server::workbench::recorder::now_ms();
    let job = Job {
        id,
        kind: Kind::Trim,
        status: Status::Ready,
        clip_ids: vec![clip],
        created_at: latest.map_or(now, |latest| now.max(latest.saturating_add(1))),
        error: None,
        progress: None,
        cut: None,
        selection: Some(selection),
        format: Container::Mp4,
        fade_out: false,
    };
    save(root, &job).await?;
    let _ = entry.keep();
    Ok(job)
}

pub async fn start(
    root: PathBuf,
    clip_ids: Vec<i64>,
    inputs: Vec<Input>,
    format: Container,
    fade_out: bool,
) -> Result<Job, StartError> {
    let slot = QUEUE.try_acquire().map_err(|_| StartError::Busy)?;
    fs::create_dir_all(jobs_dir(&root))
        .await
        .map_err(StartError::Storage)?;
    let job_dir = tempfile::Builder::new()
        .prefix("job-")
        .tempdir_in(jobs_dir(&root))
        .map_err(StartError::Storage)?;
    let id = job_dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    // Snapshot the inputs before queuing so a re-export or removal of the original
    // cannot change the video behind this job.
    let scratch = tempfile::Builder::new()
        .prefix("work-")
        .tempdir_in(job_dir.path())
        .map_err(StartError::Storage)?;
    let mut snapshots = Vec::with_capacity(inputs.len());
    for (i, input) in inputs.into_iter().enumerate() {
        let snapshot = scratch.path().join(format!("source-{i}"));
        if fs::hard_link(&input.path, &snapshot).await.is_err() {
            fs::copy(&input.path, &snapshot)
                .await
                .map_err(StartError::Storage)?;
        }
        snapshots.push(Input {
            path: snapshot,
            range: input.range,
        });
    }
    let job = Job {
        id,
        kind: Kind::Join,
        status: Status::Queued,
        clip_ids,
        created_at: crate::server::workbench::recorder::now_ms(),
        error: None,
        progress: None,
        cut: None,
        selection: None,
        format,
        fade_out,
    };
    ACTIVE.lock().unwrap().insert(
        job.id.clone(),
        Progress {
            stage: "等待处理".into(),
            ratio: None,
        },
    );
    if let Err(error) = save(&root, &job).await {
        ACTIVE.lock().unwrap().remove(&job.id);
        return Err(StartError::Storage(error));
    }
    let _ = job_dir.keep();
    let mut running = job.clone();
    tokio::spawn(async move {
        let _slot = slot;
        let _worker = STREAM_COPY
            .acquire()
            .await
            .expect("copy semaphore is never closed");
        running.status = Status::Running;
        progress(&running.id, "检查切片", None);
        if let Err(error) = save(&root, &running).await {
            warn!(%error, job = %running.id, "无法写入切片处理状态");
        }
        let target = root.join(running.filename());
        match join_streams(&running, &snapshots, scratch.path(), &target).await {
            Ok(()) => running.status = Status::Ready,
            Err(error) => {
                running.status = Status::Failed;
                running.error = Some(error);
            }
        }
        if let Err(error) = save(&root, &running).await {
            warn!(%error, job = %running.id, "无法写入切片处理结果");
        }
        ACTIVE.lock().unwrap().remove(&running.id);
    });
    Ok(job)
}

#[derive(Deserialize)]
struct Probe {
    streams: Vec<Stream>,
    format: MediaFormat,
}

#[derive(Deserialize)]
struct Stream {
    codec_type: String,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u32>,
    profile: Option<String>,
    duration: Option<String>,
    avg_frame_rate: Option<String>,
}

#[derive(Deserialize)]
struct MediaFormat {
    duration: Option<String>,
    start_time: Option<String>,
}

fn probe_command() -> Command {
    let binary = if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    };
    let sibling = crate::tools::ffmpeg().with_file_name(binary);
    let executable = if sibling.is_file() {
        sibling
    } else {
        PathBuf::from(binary)
    };
    let mut command = Command::new(executable);
    command.args([
        "-protocol_whitelist",
        "file",
        "-format_whitelist",
        "flv,mov,matroska,mpegts",
    ]);
    command
}

async fn probe(path: &Path) -> Result<Probe, String> {
    let output = probe_command()
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,codec_name,width,height,pix_fmt,sample_rate,channels,profile,duration,avg_frame_rate:format=duration,start_time",
            "-of",
            "json",
        ])
        .arg(path)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("无法运行 FFprobe：{e}"))?;
    if !output.status.success() {
        return Err("无法读取源切片".into());
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("无法读取切片信息：{e}"))
}

pub(super) async fn ffmpeg(command: &mut Command) -> Result<(), String> {
    let output = command
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("无法运行 FFmpeg：{e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(ffmpeg_message(
            &String::from_utf8_lossy(&output.stderr),
            Some(output.status),
        ))
    }
}

pub(super) async fn encode_with_progress(
    command: &mut Command,
    duration: f64,
    update: impl Fn(f64),
) -> Result<(), String> {
    let mut child = command
        .args(["-nostats", "-progress", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("无法运行 FFmpeg：{error}"))?;
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut stderr = child.stderr.take().unwrap();
    let read_progress = async {
        let mut last = 0.0_f64;
        while let Some(line) = lines.next_line().await? {
            if let Some(value) = line.strip_prefix("out_time_us=")
                && let Ok(micros) = value.parse::<i64>()
            {
                let portion = (micros as f64 / 1_000_000.0 / duration).clamp(0.0, 1.0);
                last = last.max(portion);
                update(last);
            }
        }
        Ok::<_, io::Error>(())
    };
    let mut diagnostics = Vec::new();
    let (read, errors, status) = tokio::join!(
        read_progress,
        stderr.read_to_end(&mut diagnostics),
        child.wait()
    );
    let status = status.map_err(|error| error.to_string())?;
    if !status.success() {
        return Err(ffmpeg_message(
            &String::from_utf8_lossy(&diagnostics),
            Some(status),
        ));
    }
    read.and(errors.map(|_| ()))
        .map_err(|error| error.to_string())
}

pub(super) async fn has_audio(path: &Path) -> Result<bool, String> {
    Ok(probe(path).await?.streams.iter().any(|stream| stream.codec_type == "audio"))
}

pub(super) async fn video_duration_ms(path: &Path) -> Result<i64, String> {
    let media = probe(path).await?;
    if !media
        .streams
        .iter()
        .any(|stream| stream.codec_type == "video" && stream.width.unwrap_or(0) > 0)
    {
        return Err("文件没有可读取的视频轨".into());
    }
    Ok((duration(&media)? * 1_000.0).round() as i64)
}

fn duration(media: &Probe) -> Result<f64, String> {
    media
        .format
        .duration
        .as_deref()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or_else(|| "无法读取切片时长".into())
}

fn video_and_audio(media: &Probe) -> Result<(&Stream, Option<&Stream>), String> {
    let video = media
        .streams
        .iter()
        .find(|stream| stream.codec_type == "video" && stream.codec_name.is_some())
        .ok_or("没有可复制的视频轨")?;
    let audio = media
        .streams
        .iter()
        .find(|stream| stream.codec_type == "audio");
    Ok((video, audio))
}

fn compatible(
    first: (&Stream, Option<&Stream>),
    next: (&Stream, Option<&Stream>),
) -> Result<(), &'static str> {
    let (a, b) = (first.0, next.0);
    if (
        a.codec_name.as_ref(),
        a.width,
        a.height,
        a.pix_fmt.as_ref(),
        a.profile.as_ref(),
    ) != (
        b.codec_name.as_ref(),
        b.width,
        b.height,
        b.pix_fmt.as_ref(),
        b.profile.as_ref(),
    ) {
        return Err("视频编码、分辨率或画面格式不同");
    }
    let audio_signature = |track: &Stream| {
        (
            track.codec_name.clone(),
            track.profile.clone(),
            track.sample_rate.clone(),
            track.channels,
        )
    };
    if first.1.map(audio_signature) != next.1.map(audio_signature) {
        return Err("音轨编码、采样率或声道不同");
    }
    Ok(())
}

fn copy_command() -> Command {
    let mut command = crate::tools::ffmpeg_command();
    command.args([
        "-hide_banner",
        "-loglevel",
        "warning",
        "-nostdin",
        "-protocol_whitelist",
        "file",
    ]);
    command
}

async fn copy_streams(command: &mut Command) -> Result<(), String> {
    let output = command
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| format!("无法运行 FFmpeg：{error}"))?;
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(ffmpeg_message(&diagnostics, Some(output.status)));
    }
    if diagnostics.lines().any(|line| {
        let line = line.to_ascii_lowercase();
        line.contains("non-monoton")
            || line.contains("non monoton")
            || line.contains("timestamps are unset")
    }) {
        return Err("音视频时间戳不连续，无法可靠地直接复制".into());
    }
    Ok(())
}

async fn remux(input: &Path, output: &Path) -> Result<(), String> {
    copy_streams(
        copy_command()
            .args(["-format_whitelist", "flv,mov,matroska,mpegts", "-i"])
            .arg(input)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "0:a:0?",
                "-c",
                "copy",
                // Discard audio priming packets that end before playback
                // starts, so they cannot overlap the preceding clip.
                "-bsf:a",
                "noise=amount=0:drop='lt(pts,0)*gt(duration,0)*lte(pts+duration,0)'",
                "-video_track_timescale",
                "16000",
            ])
            .arg(output),
    )
    .await
}

async fn join_streams(
    job: &Job,
    inputs: &[Input],
    scratch: &Path,
    target: &Path,
) -> Result<(), String> {
    let format = job.format;
    let copy_ratio = if job.fade_out { 0.15 } else { 0.9 };
    let first = probe(&inputs[0].path).await?;
    let signature = video_and_audio(&first)?;
    let mut list = String::new();
    for (index, input) in inputs.iter().enumerate() {
        if index > 0 {
            let next = probe(&input.path)
                .await
                .map_err(|error| format!("第 {} 段：{error}", index + 1))?;
            let tracks =
                video_and_audio(&next).map_err(|error| format!("第 {} 段：{error}", index + 1))?;
            compatible(signature, tracks)
                .map_err(|error| format!("第 {} 段与第 1 段的{error}，无法直接拼接", index + 1))?;
        }
        progress(
            &job.id,
            &format!("准备第 {} / {} 段", index + 1, inputs.len()),
            Some(index as f64 / inputs.len() as f64 * copy_ratio),
        );
        let name = format!("part-{index}.mp4");
        let part = scratch.join(&name);
        let result = match input.range {
            Some(range) => {
                let source = scratch.join(format!("range-source-{index}.mp4"));
                remux(&input.path, &source).await?;
                copy_range(&source, range, &part).await
            }
            None => remux(&input.path, &part).await,
        };
        result.map_err(|error| format!("第 {} 段无法直接复制：{error}", index + 1))?;
        list.push_str(&format!("file '{name}'\n"));
    }
    let manifest = scratch.join("concat.txt");
    fs::write(&manifest, list)
        .await
        .map_err(|error| error.to_string())?;
    let output = scratch.join(format!("joined.{}", format.extension()));
    progress(&job.id, "直接拼接成片", Some(copy_ratio));
    let mut command = copy_command();
    command
        .args(["-f", "concat", "-safe", "1", "-i"])
        .arg(manifest)
        .args(["-map", "0:v:0", "-map", "0:a:0?", "-c", "copy"]);
    match format {
        Container::Mp4 | Container::M4v | Container::Mov if !job.fade_out => {
            command.args(["-movflags", "+faststart"]);
        }
        Container::Flv => {
            command.args(["-flvflags", "add_keyframe_index"]);
        }
        _ => {}
    }
    copy_streams(command.args(["-f", format.muxer()]).arg(&output)).await?;
    let output = if job.fade_out {
        let faded = scratch.join("fade.mp4");
        encode_fade(&job.id, &output, &faded, copy_ratio).await?;
        faded
    } else {
        output
    };
    video_duration_ms(&output).await?;
    fs::rename(output, target)
        .await
        .map_err(|error| format!("无法保存成片：{error}"))
}

async fn encode_fade(id: &str, input: &Path, output: &Path, copy_ratio: f64) -> Result<(), String> {
    let media = probe(input).await?;
    let (video, audio) = video_and_audio(&media)?;
    let total = duration(&media)?;
    let video_length = video
        .duration
        .as_deref()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(total);
    let frame_interval = video
        .avg_frame_rate
        .as_deref()
        .and_then(|value| value.split_once('/'))
        .and_then(|(num, den)| Some(den.parse::<f64>().ok()? / num.parse::<f64>().ok()?))
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(1.0 / 30.0);
    // End the fade just before the last frame, without extending the cut.
    let black_at = (video_length - frame_interval.max(0.1)).max(video_length * 0.5);
    let fade_duration = (video_length * 0.16).clamp(0.45, 1.2).min(black_at);
    let fade_start = black_at - fade_duration;
    let filter = format!(
        "pad=ceil(iw/2)*2:ceil(ih/2)*2,fade=t=out:st={fade_start:.6}:d={fade_duration:.6}"
    );
    let pixels = u64::from(video.width.unwrap_or(1920)) * u64::from(video.height.unwrap_or(1080));
    let bitrate = [
        (1280 * 720, 3000),
        (1920 * 1080, 6000),
        (2560 * 1440, 10000),
        (4096 * 2160, 20000),
    ]
    .into_iter()
    .find_map(|(limit, rate)| (pixels <= limit).then_some(rate))
    .unwrap_or(40000);
    let render_with = |encoder: &[&str]| {
        let mut command = copy_command();
        command
            .args(["-y", "-threads", "4", "-i"])
            .arg(input)
            .args(["-map", "0:v:0", "-map", "0:a:0?", "-filter_threads", "2", "-vf"])
            .arg(&filter)
            .args(encoder)
            .args(["-pix_fmt", "yuv420p", "-b:v"])
            .arg(format!("{bitrate}k"))
            .arg("-maxrate")
            .arg(format!("{}k", bitrate * 2))
            .arg("-bufsize")
            .arg(format!("{}k", bitrate * 2));
        if audio.is_some_and(|track| track.codec_name.as_deref() != Some("aac")) {
            command.args(["-c:a", "aac", "-b:a", "192k"]);
        } else {
            command.args(["-c:a", "copy"]);
        }
        command.args(["-movflags", "+faststart"]).arg(output);
        command
    };
    let update = |ratio| {
        progress(id, "编码结尾淡出 MP4", Some(copy_ratio + ratio * (0.99 - copy_ratio)));
    };
    update(0.0);
    if cfg!(target_os = "macos") {
        let mut command = render_with(&["-c:v", "h264_videotoolbox"]);
        match encode_with_progress(&mut command, total, update).await {
            Ok(()) => return Ok(()),
            Err(error) => tracing::info!(%error, "结尾淡出改用软件编码"),
        }
    }
    let mut command = render_with(&["-c:v", "libx264", "-threads", "4", "-preset", "veryfast"]);
    encode_with_progress(&mut command, total, update).await
}

async fn cut_boundaries(input: &Path, requested: Range) -> Result<(f64, f64, Option<f64>), String> {
    let media = probe(input).await?;
    let length = duration(&media)?;
    let origin = media
        .format
        .start_time
        .as_deref()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0);
    let from = requested.start_ms as f64 / 1000.0;
    let to = requested.end_ms as f64 / 1000.0;
    if from >= length || to > length + 0.1 {
        return Err("裁切时间超出切片时长".into());
    }
    let output = probe_command()
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_packets",
            "-show_entries",
            "packet=pts_time,dts_time,flags",
            "-of",
            "csv=p=0",
        ])
        .arg(input)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| format!("无法读取关键帧：{error}"))?;
    if !output.status.success() {
        return Err("无法读取切片关键帧".into());
    }
    let mut start = 0.0_f64;
    let mut following_keyframe: Option<f64> = None;
    let end = to.min(length);
    let mut end_dts: Option<f64> = None;
    let mut keyframes = false;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split(',');
        let (Some(time), Some(dts), Some(flags)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Ok(pts) = time.parse::<f64>() else {
            continue;
        };
        if !pts.is_finite() {
            continue;
        }
        let time = pts - origin;
        if flags.starts_with('K') {
            keyframes = true;
            if time <= from + 0.000_001 {
                start = start.max(time);
            } else if time < end {
                following_keyframe =
                    Some(following_keyframe.map_or(time, |previous| previous.min(time)));
            }
        }
        if time >= end - 0.000_001
            && let Ok(dts) = dts.parse::<f64>()
            && dts.is_finite()
        {
            let dts = dts - origin;
            end_dts = Some(end_dts.map_or(dts, |previous| previous.min(dts)));
        }
    }
    if let Some(following) = following_keyframe
        && following - from < from - start
    {
        start = following;
    }
    if !keyframes || end <= start {
        return Err("没有找到可用的关键帧，无法直接裁切".into());
    }
    Ok((start, end, end_dts.map(|dts| dts - start)))
}

async fn copy_range(source: &Path, requested: Range, output: &Path) -> Result<(), String> {
    let (start, end, end_dts) = cut_boundaries(source, requested).await?;
    let length = end - start;
    let mut command = copy_command();
    if start > 0.0 {
        command.args(["-ss", &format!("{start:.6}")]);
    }
    command.arg("-i").arg(source).args([
        "-t",
        &format!("{length:.6}"),
        "-map",
        "0:v:0",
        "-map",
        "0:a:0?",
        "-c",
        "copy",
        "-video_track_timescale",
        "16000",
    ]);
    // Keep a complete decode-order prefix so no retained frame loses a reference.
    if let Some(dts) = end_dts {
        command.args([
            "-bsf:v",
            &format!("noise=amount=0:drop='gte(dts*tb,{:.6})'", dts - 0.000_001),
        ]);
    }
    if start > 0.0 {
        // Audio packets read ahead of a seek are outside this selected range.
        command.args(["-bsf:a", "noise=amount=0:drop='lt(pts,0)'"]);
    }
    copy_streams(command.arg(output)).await?;
    let actual_duration = video_duration_ms(output).await?;
    if (actual_duration as f64 / 1000.0 - length).abs() > 0.25 {
        return Err("裁切结果的时长超出选定范围，无法拼接".into());
    }
    Ok(())
}
