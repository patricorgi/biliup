use super::{library, subtitles};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use tokio::{fs, process::Command, sync::oneshot};

const FONT_FILE: &str = "SmileySans-Oblique.otf";
const FILTER: &str = concat!(
    "subtitles=filename=captions.srt:fontsdir=fonts:force_style='",
    "PlayResX=448,PlayResY=252,FontName=SmileySans-Oblique,FontSize=24,",
    "PrimaryColour=&H00FCFEFF,OutlineColour=&H00191319,",
    "BorderStyle=1,Outline=1.25,Shadow=0,Alignment=2,MarginV=29,MarginL=20,MarginR=20'"
);
static STATES: LazyLock<Mutex<HashMap<PathBuf, Entry>>> = LazyLock::new(Mutex::default);

struct Entry {
    state: State,
    cancel: Option<oneshot::Sender<()>>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum State {
    Queued,
    Running { ratio: f64 },
    Cancelling,
    Cancelled,
    Ready,
    Failed { error: String },
}

impl State {
    fn active(&self) -> bool {
        matches!(self, Self::Queued | Self::Running { .. } | Self::Cancelling)
    }
}

pub enum StartError {
    Busy,
    MissingSrt,
    MissingFont,
}

pub fn path(video: &Path) -> PathBuf {
    let mut name = video
        .file_stem()
        .expect("managed MP4 filename")
        .to_os_string();
    name.push("-带字幕.mp4");
    video.with_file_name(name)
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    meta.modified().ok()
}

pub fn current_path(video: &Path) -> Option<PathBuf> {
    let target = path(video);
    (modified(&target)? >= modified(&video.with_extension("srt"))?).then_some(target)
}

fn current_file(video: &Path) -> bool {
    current_path(video).is_some()
}

pub fn in_progress(video: &Path) -> bool {
    STATES
        .lock()
        .unwrap()
        .get(&path(video))
        .is_some_and(|entry| entry.state.active())
}

pub fn status(video: &Path) -> Option<State> {
    let target = path(video);
    STATES
        .lock()
        .unwrap()
        .get(&target)
        .map(|entry| entry.state.clone())
        .or_else(|| current_file(video).then_some(State::Ready))
}

pub fn cancel(video: &Path) -> Option<State> {
    let target = path(video);
    let mut states = STATES.lock().unwrap();
    if let Some(entry) = states.get_mut(&target) {
        if let Some(cancel) = entry.cancel.take() {
            entry.state = State::Cancelling;
            let _ = cancel.send(());
        }
        return Some(entry.state.clone());
    }
    current_file(video).then_some(State::Ready)
}

pub async fn start(video: PathBuf, job_dir: PathBuf) -> Result<State, StartError> {
    let srt = video.with_extension("srt");
    let target = path(&video);
    let has_srt = subtitles::exists(&srt).await;
    let font = Path::new("data/fonts").join(FONT_FILE);
    let has_font = fs::metadata(&font).await.is_ok_and(|meta| meta.is_file());
    let mut states = STATES.lock().unwrap();
    if let Some(entry) = states.get(&target).filter(|entry| entry.state.active()) {
        return Ok(entry.state.clone());
    }
    if current_file(&video) {
        return Ok(State::Ready);
    }
    if !has_srt {
        return Err(StartError::MissingSrt);
    }
    if !has_font {
        return Err(StartError::MissingFont);
    }
    if states.values().any(|entry| entry.state.active()) {
        return Err(StartError::Busy);
    }
    let (cancel, cancelled) = oneshot::channel();
    states.insert(
        target.clone(),
        Entry {
            state: State::Queued,
            cancel: Some(cancel),
        },
    );
    tokio::spawn(async move {
        let update = |ratio: f64| {
            if let Some(entry) = STATES.lock().unwrap().get_mut(&target)
                && !matches!(entry.state, State::Cancelling)
            {
                entry.state = State::Running {
                    ratio: ratio.min(0.99),
                };
            }
        };
        let result = tokio::select! {
            biased;
            _ = cancelled => None,
            result = async {
                let _encoder = library::ENCODER.acquire().await.expect("encoder remains open");
                update(0.0);
                render(&video, &srt, &font, &job_dir, update).await
            } => Some(result),
        };
        let mut states = STATES.lock().unwrap();
        let entry = states
            .get_mut(&target)
            .expect("active export is registered");
        entry.cancel = None;
        if matches!(entry.state, State::Cancelling) || result.is_none() {
            entry.state = State::Cancelled;
            return;
        }
        let result = result.expect("export was not cancelled").and_then(|work| {
            std::fs::rename(work.path().join("rendered.mp4"), &target)
                .map_err(|error| error.to_string())
        });
        match result {
            Ok(()) => {
                states.remove(&target);
            }
            Err(error) => {
                tracing::warn!(%error, "内嵌字幕导出失败");
                entry.state = State::Failed { error };
            }
        }
    });
    Ok(State::Queued)
}

fn executable() -> PathBuf {
    if let Some(path) = std::env::var_os("BILIUP_SUBTITLE_FFMPEG") {
        return path.into();
    }
    for path in [
        "/opt/homebrew/opt/ffmpeg-full/bin/ffmpeg",
        "/usr/local/opt/ffmpeg-full/bin/ffmpeg",
    ] {
        if cfg!(target_os = "macos") && Path::new(path).is_file() {
            return path.into();
        }
    }
    crate::tools::ffmpeg()
}

async fn render(
    video: &Path,
    srt: &Path,
    font: &Path,
    job_dir: &Path,
    update: impl Fn(f64),
) -> Result<tempfile::TempDir, String> {
    let duration = library::video_duration_ms(video).await? as f64 / 1000.0;
    let work = tempfile::Builder::new()
        .prefix("hardsubs-")
        .tempdir_in(job_dir)
        .map_err(|error| error.to_string())?;
    let fonts = work.path().join("fonts");
    fs::create_dir(&fonts)
        .await
        .map_err(|error| error.to_string())?;
    fs::copy(font, fonts.join(FONT_FILE))
        .await
        .map_err(|error| error.to_string())?;
    fs::copy(srt, work.path().join("captions.srt"))
        .await
        .map_err(|error| error.to_string())?;
    let program = executable();
    let render_with = |encoder: &[&str]| {
        let mut command = Command::new(&program);
        command
            .current_dir(work.path())
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-protocol_whitelist",
                "file",
                "-threads",
                "4",
                "-i",
            ])
            .arg(video)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "0:a?",
                "-filter_threads",
                "2",
                "-vf",
                FILTER,
            ])
            .args(encoder)
            .args([
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "copy",
                "-movflags",
                "+faststart",
                "rendered.mp4",
            ]);
        command
    };
    let mut hardware_ready = false;
    if cfg!(target_os = "macos") {
        let mut command = render_with(&["-c:v", "h264_videotoolbox", "-q:v", "73"]);
        match library::encode_with_progress(&mut command, duration, &update).await {
            Ok(()) => hardware_ready = true,
            Err(error) => tracing::info!(%error, "内嵌字幕改用软件编码"),
        }
    }
    if !hardware_ready {
        update(0.0);
        let mut command = render_with(&[
            "-c:v", "libx264", "-threads", "4", "-preset", "veryfast", "-crf", "18",
        ]);
        library::encode_with_progress(&mut command, duration, update).await?;
    }
    Ok(work)
}
