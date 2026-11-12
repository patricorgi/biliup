use super::{captions, library};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use tokio::{fs, process::Command, sync::Mutex};

const DEFAULT_MODEL: &str = "data/models/ggml-large-v3-turbo-q5_0.bin";
const DEFAULT_VAD_MODEL: &str = "data/models/ggml-silero-v6.2.0.bin";
static STATES: LazyLock<Mutex<HashMap<PathBuf, State>>> = LazyLock::new(Mutex::default);
static RECOGNITION: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
pub static EDIT_LOCK: Mutex<()> = Mutex::const_new(());
const NO_SPEECH: &str = "未识别到可生成字幕的语音";
const REFERENCE: &str = "subtitle-reference.srt";

#[derive(Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum State {
    Running,
    Ready,
    Failed { error: String },
}

pub enum StartError {
    Busy,
    MissingModel,
}

pub async fn status(path: &Path) -> Option<State> {
    if let Some(state) = STATES.lock().await.get(path).cloned() {
        return Some(state);
    }
    exists(path).await.then_some(State::Ready)
}

pub async fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .await
        .is_ok_and(|meta| meta.is_file() && meta.len() > 0)
}

pub fn webvtt(srt: &str) -> String {
    static TIMING: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^(\d{2,}:\d{2}:\d{2}),(\d{3}) --> (\d{2,}:\d{2}:\d{2}),(\d{3})$")
            .expect("valid subtitle timing pattern")
    });
    let mut vtt = String::from("WEBVTT\n\n");
    for line in srt.trim_start_matches('\u{feff}').lines() {
        if TIMING.is_match(line) {
            vtt.push_str(&line.replace(',', "."));
        } else {
            vtt.push_str(
                &line
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;"),
            );
        }
        vtt.push('\n');
    }
    vtt
}

pub async fn start(video: PathBuf, job_dir: PathBuf) -> Result<State, StartError> {
    let target = video.with_extension("srt");
    let mut states = STATES.lock().await;
    if matches!(states.get(&target), Some(State::Running)) {
        return Ok(State::Running);
    }
    if exists(&target).await {
        return Ok(State::Ready);
    }
    if states.values().any(|state| matches!(state, State::Running)) {
        return Err(StartError::Busy);
    }
    let (model, vad_model) = models();
    for path in [&model, &vad_model] {
        if !fs::metadata(path).await.is_ok_and(|meta| meta.is_file()) {
            return Err(StartError::MissingModel);
        }
    }
    states.insert(target.clone(), State::Running);
    tokio::spawn(async move {
        let result = generate(&video, &job_dir, &model, &vad_model, &target).await;
        let mut states = STATES.lock().await;
        match result {
            Ok(()) => {
                states.remove(&target);
            }
            Err(error) => {
                states.insert(target, State::Failed { error });
            }
        }
    });
    Ok(State::Running)
}

async fn generate(
    video: &Path,
    job_dir: &Path,
    model: &Path,
    vad_model: &Path,
    target: &Path,
) -> Result<(), String> {
    let reference = recognize(video, job_dir, model, vad_model).await?;
    let lines = captions::automatic(&reference);
    let duration = library::video_duration_ms(video).await?;
    let cues = captions::align(&reference, &lines, duration);
    if cues.is_empty() {
        return Err(NO_SPEECH.into());
    }
    save_file(&job_dir.join(REFERENCE), &captions::render(&reference)).await?;
    save_file(target, &captions::render(&cues)).await
}

fn models() -> (PathBuf, PathBuf) {
    let model = std::env::var_os("BILIUP_WHISPER_MODEL")
        .map_or_else(|| PathBuf::from(DEFAULT_MODEL), PathBuf::from);
    let vad = std::env::var_os("BILIUP_WHISPER_VAD_MODEL")
        .map_or_else(|| PathBuf::from(DEFAULT_VAD_MODEL), PathBuf::from);
    (model, vad)
}

pub(crate) async fn transcript_cues(video: &Path, scratch: &Path, duration: i64) -> Result<Vec<TranscriptCue>, String> {
    if !library::has_audio(video).await? {
        return Ok(Vec::new());
    }
    let (model, vad_model) = models();
    if !model.is_file() || !vad_model.is_file() {
        return Err("本机尚未配置语音识别模型".into());
    }
    let reference = match recognize(video, scratch, &model, &vad_model).await {
        Err(error) if error == NO_SPEECH => return Ok(Vec::new()),
        result => result?,
    };
    let lines = captions::automatic_with_limit(&reference, 25);
    Ok(captions::align(&reference, &lines, duration).into_iter().map(|cue| TranscriptCue {
        at_ms: cue.start, end_ms: Some(cue.end), text: cue.text,
    }).collect())
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct TranscriptCue {
    pub at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<i64>,
    pub text: String,
}

async fn recognize(
    video: &Path,
    job_dir: &Path,
    model: &Path,
    vad_model: &Path,
) -> Result<Vec<captions::Cue>, String> {
    let _worker = RECOGNITION
        .acquire()
        .await
        .expect("recognizer remains open");
    let temp = tempfile::Builder::new()
        .prefix("subtitles-")
        .tempdir_in(job_dir)
        .map_err(storage_error)?;
    let audio = temp.path().join("audio.wav");
    let result = temp.path().join("result");
    let mut ffmpeg = Command::new("ffmpeg");
    ffmpeg
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(video)
        // Keep the audio on the MP4 timeline, including any leading gap.
        .args([
            "-map",
            "0:a:0",
            "-af",
            "aresample=async=1:first_pts=0",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
        ])
        .arg(&audio);
    run(
        ffmpeg,
        "服务器未安装 FFmpeg",
        "无法读取成片音轨，请确认 MP4 含有可播放的音频",
    )
    .await?;

    let executable = std::env::var_os("BILIUP_WHISPER_CLI").unwrap_or_else(|| "whisper-cli".into());
    let mut detector = Command::new("whisper-vad-speech-segments");
    configure_workers(&mut detector);
    detector
        .args(["-np", "-t", "1", "-vm"])
        .arg(vad_model)
        .arg("-f")
        .arg(&audio);
    let speech = run_capture(
        detector,
        "服务器未安装语音检测工具 whisper-vad-speech-segments",
        "无法读取成片的语音时间点",
    )
    .await?;
    let intervals: Vec<_> = String::from_utf8_lossy(&speech)
        .lines()
        .filter_map(|line| {
            let (_, span) = line.split_once(": start = ")?;
            let (start, end) = span.split_once(", end = ")?;
            let ms = |value: &str| {
                value
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|time| time.is_finite())
                    .map(|time| (time * 10.0).round() as i64)
            };
            Some((ms(start)?, ms(end)?))
        })
        .collect();
    if intervals.is_empty() {
        return Err(NO_SPEECH.into());
    }
    let mut whisper = Command::new(executable);
    configure_workers(&mut whisper);
    whisper
        .arg("-m")
        .arg(model)
        .arg("-f")
        .arg(audio)
        .args(["-l", "zh", "--prompt", "以下是简体中文字幕。", "-ml", "8"])
        .args(["-osrt", "-np", "-of"])
        .arg(&result);
    run(
        whisper,
        "服务器未安装 whisper-cli",
        "本机语音识别失败，请查看服务器日志后重试",
    )
    .await?;

    let srt = result.with_extension("srt");
    if !exists(&srt).await {
        return Err(NO_SPEECH.into());
    }
    let simplified = temp.path().join("simplified.srt");
    let text = simplify(&srt, &simplified).await?;
    let mut reference = Vec::new();
    let mut speech_index = 0;
    for mut cue in captions::parse(&text) {
        while speech_index < intervals.len() && intervals[speech_index].1 <= cue.start {
            speech_index += 1;
        }
        let mut overlaps = intervals[speech_index..]
            .iter()
            .take_while(|(start, _)| *start < cue.end);
        if let Some(&(start, end)) = overlaps.next() {
            cue.start = cue.start.max(start);
            cue.end = cue
                .end
                .min(overlaps.last().map_or(end, |interval| interval.1));
            if cue.end > cue.start {
                reference.push(cue);
            }
        }
    }
    if reference.is_empty() {
        return Err(NO_SPEECH.into());
    }
    Ok(reference)
}

#[derive(Deserialize)]
struct WordTiming {
    transcription: Vec<WordSegment>,
}

#[derive(Deserialize)]
struct WordSegment {
    tokens: Vec<WordToken>,
}

#[derive(Deserialize)]
struct WordToken {
    text: String,
    offsets: WordOffsets,
    p: f64,
}

#[derive(Deserialize)]
struct WordOffsets {
    from: i64,
    to: i64,
}

async fn recover_gap_words(
    video: &Path,
    scratch: &Path,
    windows: &[(i64, i64)],
) -> Result<Vec<captions::Cue>, String> {
    let (model, _) = models();
    let _worker = RECOGNITION
        .acquire()
        .await
        .expect("recognizer remains open");
    let mut clips = Vec::new();
    for (index, &(left, right)) in windows.iter().enumerate() {
        let offset = (left - 600).max(0);
        let audio = scratch.join(format!("gap-{index}.wav"));
        let mut ffmpeg = Command::new("ffmpeg");
        ffmpeg
            .args(["-nostdin", "-v", "error", "-ss"])
            .arg(format!("{:.3}", offset as f64 / 1_000.0))
            .arg("-i")
            .arg(video)
            .arg("-t")
            .arg(format!("{:.3}", (right + 600 - offset) as f64 / 1_000.0))
            .args([
                "-map",
                "0:a:0",
                "-af",
                "aresample=async=1:first_pts=0",
                "-ac",
                "1",
                "-ar",
                "16000",
                "-c:a",
                "pcm_s16le",
            ])
            .arg(&audio);
        run(ffmpeg, "服务器未安装 FFmpeg", "无法读取待复查的音频").await?;
        clips.push((audio, offset, left, right));
    }

    let executable = std::env::var_os("BILIUP_WHISPER_CLI").unwrap_or_else(|| "whisper-cli".into());
    let mut whisper = Command::new(executable);
    configure_workers(&mut whisper);
    whisper.arg("-m").arg(model).args([
        "-l",
        "zh",
        "--prompt",
        "以下是简体中文字幕。",
        "-ml",
        "8",
        "-ojf",
        "-np",
    ]);
    for (audio, _, _, _) in &clips {
        whisper.arg("-f").arg(audio);
    }
    run(whisper, "服务器未安装 whisper-cli", "无法复查字幕时间点").await?;

    let mut recovered = Vec::new();
    for (audio, offset, left, right) in clips {
        let json = fs::read(audio.with_extension("wav.json"))
            .await
            .map_err(storage_error)?;
        let timing: WordTiming =
            serde_json::from_slice(&json).map_err(|_| "无法读取逐字识别时间".to_owned())?;
        for token in timing
            .transcription
            .into_iter()
            .flat_map(|segment| segment.tokens)
        {
            let text = token.text.trim();
            if token.p < 0.33 || text.starts_with("[_") || !text.chars().any(char::is_alphanumeric)
            {
                continue;
            }
            let start = offset + token.offsets.from;
            let end = offset + token.offsets.to;
            let length = text.chars().count() as i64;
            if start >= left && start < right && end - start >= 30 && end - start <= length * 750 {
                recovered.push(captions::Cue {
                    start,
                    end: end.min(right),
                    text: text.to_owned(),
                });
            }
        }
    }
    if recovered.is_empty() {
        return Ok(recovered);
    }
    let source = scratch.join("recovered.srt");
    fs::write(&source, captions::render(&recovered))
        .await
        .map_err(storage_error)?;
    let simplified = simplify(&source, &scratch.join("recovered-simplified.srt")).await?;
    Ok(captions::parse(&simplified))
}

fn configure_workers(command: &mut Command) {
    // Prevent OpenMP starvation under macOS background scheduling.
    if cfg!(target_os = "macos") {
        command
            .env("OMP_WAIT_POLICY", "PASSIVE")
            .env("OMP_THREAD_LIMIT", "1");
    }
}

async fn simplify(source: &Path, target: &Path) -> Result<String, String> {
    let mut opencc = Command::new("opencc");
    opencc
        .args(["-c", "t2s.json", "-i"])
        .arg(source)
        .arg("-o")
        .arg(target);
    run(
        opencc,
        "服务器未安装简繁转换工具 OpenCC",
        "无法将识别结果转成简体中文，请查看服务器日志后重试",
    )
    .await?;
    fs::read_to_string(target).await.map_err(storage_error)
}

#[derive(Serialize)]
pub struct Draft {
    text: String,
    revision: String,
}

fn revision(srt: &str) -> String {
    format!("{:x}", Sha256::digest(srt.as_bytes()))
}

pub async fn draft(video: &Path) -> io::Result<Draft> {
    let srt = fs::read_to_string(video.with_extension("srt")).await?;
    Ok(Draft {
        text: captions::parse(&srt)
            .iter()
            .flat_map(|cue| captions::lines(&cue.text))
            .collect::<Vec<_>>()
            .join("\n"),
        revision: revision(&srt),
    })
}

pub enum EditError {
    Conflict,
    Text(String),
}

pub async fn edit(
    video: &Path,
    job_dir: &Path,
    text: &str,
    expected: &str,
) -> Result<Draft, EditError> {
    let path = video.with_extension("srt");
    let current = fs::read_to_string(&path)
        .await
        .map_err(|error| EditError::Text(storage_error(error)))?;
    if revision(&current) != expected {
        return Err(EditError::Conflict);
    }
    if text.len() > 120_000 || captions::lines(text).is_empty() {
        return Err(EditError::Text("请输入有效的字幕文字".into()));
    }
    let scratch =
        tempfile::tempdir_in(job_dir).map_err(|error| EditError::Text(storage_error(error)))?;
    let entered = scratch.path().join("entered.txt");
    fs::write(&entered, text)
        .await
        .map_err(|error| EditError::Text(storage_error(error)))?;
    let simplified = simplify(&entered, &scratch.path().join("simplified.txt"))
        .await
        .map_err(EditError::Text)?;
    let lines = captions::lines(&simplified);
    let reference_path = job_dir.join(REFERENCE);
    let mut reference = match fs::read_to_string(&reference_path).await {
        Ok(srt) => captions::parse(&srt),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let (model, vad) = models();
            let cues = recognize(video, job_dir, &model, &vad)
                .await
                .map_err(EditError::Text)?;
            save_file(&reference_path, &captions::render(&cues))
                .await
                .map_err(EditError::Text)?;
            cues
        }
        Err(error) => return Err(EditError::Text(storage_error(error))),
    };
    let duration = library::video_duration_ms(video)
        .await
        .map_err(EditError::Text)?;
    let windows = {
        let reference = reference.clone();
        let lines = lines.clone();
        tokio::task::spawn_blocking(move || {
            captions::refinement_windows(&reference, &lines, duration)
        })
        .await
        .map_err(|_| EditError::Text("字幕对齐失败，请重试".into()))?
    };
    if !windows.is_empty() {
        match recover_gap_words(video, scratch.path(), &windows).await {
            Ok(recovered) => {
                reference.extend(recovered);
                reference.sort_by_key(|cue| (cue.start, cue.end));
            }
            Err(error) => tracing::warn!(%error, "短音频复查失败，使用原始识别时间对齐"),
        }
    }
    let cues = tokio::task::spawn_blocking(move || captions::align(&reference, &lines, duration))
        .await
        .map_err(|_| EditError::Text("字幕对齐失败，请重试".into()))?;
    if cues.is_empty() {
        return Err(EditError::Text("没有可对齐的字幕".into()));
    }
    let srt = captions::render(&cues);
    if current != srt {
        save_file(&path, &srt).await.map_err(EditError::Text)?;
    }
    Ok(Draft {
        text: cues
            .into_iter()
            .map(|cue| cue.text)
            .collect::<Vec<_>>()
            .join("\n"),
        revision: revision(&srt),
    })
}

async fn save_file(path: &Path, text: &str) -> Result<(), String> {
    let stage = tempfile::NamedTempFile::new_in(path.parent().expect("managed file has a parent"))
        .map_err(storage_error)?;
    fs::write(stage.path(), text).await.map_err(storage_error)?;
    stage
        .persist(path)
        .map_err(|error| storage_error(error.error))?;
    Ok(())
}

async fn run(command: Command, missing: &str, failed: &str) -> Result<(), String> {
    run_capture(command, missing, failed).await.map(|_| ())
}

async fn run_capture(mut command: Command, missing: &str, failed: &str) -> Result<Vec<u8>, String> {
    let result = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| {
            tracing::warn!(%error, "无法启动字幕处理程序");
            if error.kind() == io::ErrorKind::NotFound {
                missing.into()
            } else {
                failed.to_string()
            }
        })?;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        tracing::warn!(status = %result.status, detail = %stderr, "字幕处理程序失败");
        return Err(failed.into());
    }
    Ok(result.stdout)
}

fn storage_error(error: io::Error) -> String {
    tracing::warn!(%error, "字幕文件操作失败");
    "无法保存字幕，请检查服务器磁盘空间和文件权限".into()
}
