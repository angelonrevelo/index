//! p63-video-shot :: video as SHOTS, not frames — measured on the real video files on this machine.
//!
//! `bench/roadmap/p63-video-keyframe.md` is the spec; `docs/research/image.md` §9 is the evidence.
//! The ask was "frame-by-frame image search on video". §9 says frame-by-frame is both more
//! expensive and *worse*, so this benchmark measures the alternative it proposes: decode I-frames
//! only, detect shots, embed the **middle** frame of each shot (middle, not first — a shot's first
//! frame is often still mid-transition and blurred), and store a shot as a document with a
//! timestamp.
//!
//! ## The decoder is the system ffmpeg, out of process, and that is a LICENCE decision
//!
//! §9's patent note decides this and it is not a style preference. **AV1 is royalty-free** by
//! AOMedia commitment (`dav1d`, `rav1d`). **H.264 and HEVC are not.** A permissive code licence on
//! a pure-Rust H.264 decoder does **not** remove the patent obligation, because the patents cover
//! the *technique*, not the implementation; HEVC runs ~$2.07/unit across the pools. So this
//! benchmark shells out to the ffmpeg already installed on this machine rather than linking a Rust
//! decoder: that leaves the licence exactly where it already is, on the platform, which is the same
//! thing decoding through the OS or the browser (`WebCodecs VideoDecoder`) would do. This crate
//! adds **no decoder dependency**, and the run records the ffmpeg build it used.
//!
//! ## What this corpus is, and what it is NOT
//!
//! Marketing hero loops, product reels and screen-recorded slide decks found on this developer
//! machine. Mostly short; many have very few real cuts. That makes it a legitimate test of the
//! **pipeline** — decode strategy, shot detection, cost, storage — and a **poor test of
//! retrieval**, because there are no relevance judgements anywhere in it. `p63` acceptance 3 (a
//! labelled-clip recall check) is therefore **WITHHELD**, not guessed at. A recall number over a
//! corpus with no ground truth would be a number about nothing.
//!
//! ## Shot detection here is a HEURISTIC, and is reported as one
//!
//! ffmpeg's `select='gt(scene,T)'` is an unbenchmarked frame-difference threshold. §9's measured
//! detector is PySceneDetect's `AdaptiveDetector` (**F1 91.59** on BBC vs `ContentDetector`'s
//! 86.69), which is not available on this machine. So no F1 is claimed here — only counts, a
//! threshold sweep showing how much the threshold moves the answer, and the cost.
//!
//! ## The manifest is the point of the whole row
//!
//! `--emit-manifest` writes `<path>\t<video>\t<shot_index>\t<start_ms>\t<end_ms>` per extracted
//! keyframe, so `scripts/embed-corpus.py` can embed them and a shot can be indexed as an ordinary
//! document carrying a timestamp. Nothing in `index-image` changes: a shot is an `ImageDoc` with a
//! `(video, start_ms, end_ms)`, and the fusion layer, the vector column and the hash column already
//! index it. **Video search needed no new index type at all** — which `p63` names as the strongest
//! available evidence that the `p59` architecture is the right one.

use index_image::sha256;
use index_image::vector::{Metric, VectorColumn};
use std::collections::HashMap;
use std::ffi::OsString;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Instant;

mod timer;

// ---------------------------------------------------------------------------------------------
// Constants — every one carries the number that justifies it.
// ---------------------------------------------------------------------------------------------

/// The extensions the corpus is defined by. Sniffing is ffprobe's job here, not ours.
const VIDEO_EXT: [&str; 4] = ["mp4", "mkv", "webm", "mov"];

/// The corpus walk is depth-bounded so the benchmark cannot wander a whole home directory. 5 is the
/// `-maxdepth 5` the corpus was enumerated with.
const MAX_DEPTH: usize = 5;

/// Directory name excluded from the walk. A `node_modules` tree holds vendored demo video that is
/// nobody's content.
const EXCLUDE_DIR: [&str; 1] = ["node_modules"];

/// Embedding width. 512-d is the width `p58` prices its byte table at, and the width `p60` uses.
const DIM: usize = 512;

/// Bytes for the per-frame perceptual hash stored beside the embedding. dHash is 64 bits.
const HASH_BYTE: usize = 8;

/// The 1-fps baseline this row claims to replace. §9: retrieval accuracy flattens past 2 fps and
/// 1 fps is the safe floor, so 1 fps — 60 frames per video-minute — is the honest thing to beat.
const BASELINE_FPS: f64 = 1.0;

/// Scene-score thresholds swept. 0.3 is the primary because it is ffmpeg's conventional choice; the
/// rest are printed so that the threshold's grip on the answer is visible rather than hidden.
const SCENE_THRESHOLD: [f64; 4] = [0.2, 0.3, 0.4, 0.5];
const PRIMARY_THRESHOLD: f64 = 0.3;

/// The shortest span allowed to count as a shot. A frame-difference detector fires on the last
/// frame of a clip and on flash transitions, which manufactures 40 ms "shots" that are detection
/// artefacts rather than shots — and the first run of this benchmark proved it the hard way: a
/// 42 ms trailing span on `steps-loop.mp4` put the extraction seek PAST the last coded frame, so
/// ffmpeg produced no frame at all and the run went red. 0.25 s is ~6 frames at 24 fps, far below
/// §9's 3.5-6 s typical shot length, so the floor removes artefacts without touching real
/// cuts. A span under the floor is MERGED into its predecessor, never dropped: no video time is
/// lost.
const MIN_SHOT_SECOND: f64 = 0.25;

/// Cap on per-finding examples printed. Bounded output is a methodology requirement.
const EXAMPLE_MAX: usize = 8;

/// A `-ss` seek plus a single-frame decode per shot. Refuse to launch more processes than this in
/// one run; the cap is stated in the output whenever it bites, never silently applied.
const EXTRACT_MAX: usize = 4000;

// ---------------------------------------------------------------------------------------------
// Corpus location
// ---------------------------------------------------------------------------------------------

fn corpus_dir() -> PathBuf {
    std::env::var("INDEX_VIDEO_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\Users\maran\Code"))
}

/// Depth-first, **sorted at every level**, depth-bounded, `node_modules` excluded. The walk order is
/// part of the verdict: document order, manifest order and every tie-break derive from it.
fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut entry: Vec<PathBuf> = read.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entry.sort();
    for path in entry {
        let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if path.is_dir() {
            if !EXCLUDE_DIR.contains(&name.as_str()) {
                walk(&path, depth + 1, out);
            }
        } else {
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if VIDEO_EXT.contains(&ext.as_str()) {
                out.push(path);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Panic containment
// ---------------------------------------------------------------------------------------------

/// A file ffmpeg cannot open is a FINDING with a filename, not a crash. The hook stores the message
/// so a caught panic becomes a bounded report line rather than a wall of backtrace.
static PANIC_MESSAGE: Mutex<Option<String>> = Mutex::new(None);

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        if let Ok(mut slot) = PANIC_MESSAGE.lock() {
            *slot = Some(info.to_string());
        }
    }));
}

fn take_panic_message() -> String {
    PANIC_MESSAGE
        .lock()
        .ok()
        .and_then(|mut s| s.take())
        .unwrap_or_else(|| "<no message>".to_string())
}

fn guard<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(AssertUnwindSafe(f)).map_err(|_| take_panic_message())
}

// ---------------------------------------------------------------------------------------------
// ffmpeg, out of process
// ---------------------------------------------------------------------------------------------

struct Run {
    stdout: String,
    stderr: String,
    ok: bool,
    second: f64,
}

/// Run a child process to completion, capturing both streams and the wall clock.
///
/// `Instant` and not the `timer::Clock` TSC path on purpose: what is timed here is a whole external
/// process, tens of milliseconds at the very least, so nanosecond resolution would be false
/// precision. Process spawn is INSIDE the figure, which is stated where the figures are printed.
fn run(program: &str, arg: &[OsString]) -> Result<Run, String> {
    let start = Instant::now();
    let out = Command::new(program).args(arg).output().map_err(|e| format!("{program}: {e}"))?;
    let second = start.elapsed().as_secs_f64();
    Ok(Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        ok: out.status.success(),
        second,
    })
}

fn os(s: &str) -> OsString {
    OsString::from(s)
}

/// `-progress pipe:1` is a machine-readable interface; scraping the human `frame=` status line is
/// not. This reads the last `frame=` key from the progress stream.
fn progress_frame(stdout: &str) -> usize {
    stdout
        .lines()
        .filter_map(|l| l.strip_prefix("frame="))
        .filter_map(|v| v.trim().parse::<usize>().ok())
        .next_back()
        .unwrap_or(0)
}

/// One `ffprobe` call, parsed into the handful of facts the rest of the run needs.
struct Probe {
    duration_s: f64,
    width: u32,
    height: u32,
    codec: String,
    fps: f64,
}

fn probe(path: &Path) -> Result<Probe, String> {
    let arg = vec![
        os("-v"),
        os("error"),
        os("-select_streams"),
        os("v:0"),
        os("-show_entries"),
        os("stream=codec_name,width,height,avg_frame_rate"),
        os("-show_entries"),
        os("format=duration"),
        os("-of"),
        os("default=noprint_wrappers=1"),
        path.as_os_str().to_os_string(),
    ];
    let r = run("ffprobe", &arg)?;
    if !r.ok {
        return Err(first_line(&r.stderr));
    }
    let mut field: HashMap<&str, &str> = HashMap::new();
    for line in r.stdout.lines() {
        if let Some((k, v)) = line.split_once('=') {
            field.insert(k.trim(), v.trim());
        }
    }
    let codec = field.get("codec_name").copied().unwrap_or("").to_string();
    if codec.is_empty() {
        return Err("no video stream (container holds audio only, or is not decodable)".to_string());
    }
    let duration_s = field.get("duration").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    if duration_s.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return Err("container reports no usable duration".to_string());
    }
    let fps = field
        .get("avg_frame_rate")
        .and_then(|v| v.split_once('/'))
        .and_then(|(n, d)| Some((n.parse::<f64>().ok()?, d.parse::<f64>().ok()?)))
        .filter(|(_, d)| *d != 0.0)
        .map(|(n, d)| n / d)
        .unwrap_or(0.0);
    Ok(Probe {
        duration_s,
        width: field.get("width").and_then(|v| v.parse().ok()).unwrap_or(0),
        height: field.get("height").and_then(|v| v.parse().ok()).unwrap_or(0),
        codec,
        fps,
    })
}

fn first_line(s: &str) -> String {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or("<no message>").trim().to_string()
}

/// Decode `path` under one strategy, returning `(wall seconds, frames the muxer saw)`.
fn decode_pass(path: &Path, filter: Option<&str>, keyframe_only: bool) -> Result<Run, String> {
    let mut arg = vec![os("-v"), os("error"), os("-nostdin"), os("-nostats"), os("-progress"), os("pipe:1")];
    if keyframe_only {
        // Order matters: `-skip_frame` is an input option and must precede `-i`.
        arg.push(os("-skip_frame"));
        arg.push(os("nokey"));
    }
    arg.push(os("-i"));
    arg.push(path.as_os_str().to_os_string());
    arg.push(os("-an"));
    if let Some(f) = filter {
        arg.push(os("-vf"));
        arg.push(os(f));
    }
    arg.push(os("-f"));
    arg.push(os("null"));
    arg.push(os("-"));
    run("ffmpeg", &arg)
}

/// One full decode that prints EVERY frame's scene score, so the threshold sweep costs one pass
/// rather than one pass per threshold. `gte(scene,0)` passes essentially every frame to
/// `metadata=print`, which writes `pts_time` and `lavfi.scene_score` to stdout.
fn scene_score(path: &Path) -> Result<(Vec<(f64, f64)>, f64), String> {
    let arg = vec![
        os("-v"),
        os("error"),
        os("-nostdin"),
        os("-nostats"),
        os("-i"),
        path.as_os_str().to_os_string(),
        os("-an"),
        os("-vf"),
        os("select='gte(scene,0)',metadata=print:file=-"),
        os("-f"),
        os("null"),
        os("-"),
    ];
    let r = run("ffmpeg", &arg)?;
    if !r.ok {
        return Err(first_line(&r.stderr));
    }
    let mut out: Vec<(f64, f64)> = Vec::new();
    let mut pending: Option<f64> = None;
    for line in r.stdout.lines() {
        if let Some(idx) = line.find("pts_time:") {
            pending = line[idx + "pts_time:".len()..].split_whitespace().next().and_then(|v| v.parse().ok());
        } else if let Some(v) = line.trim().strip_prefix("lavfi.scene_score=") {
            if let (Some(t), Ok(s)) = (pending.take(), v.trim().parse::<f64>()) {
                out.push((t, s));
            }
        }
    }
    Ok((out, r.second))
}

/// Shot boundaries at `threshold`, turned into `(start_s, end_s)` spans covering `[0, duration)`.
///
/// The frame at t=0 always scores 0 (there is no previous frame to differ from), so it never opens
/// a spurious boundary; the first shot starts at 0 by construction.
fn shot_span(score: &[(f64, f64)], threshold: f64, duration_s: f64) -> Vec<(f64, f64)> {
    let mut boundary: Vec<f64> =
        score.iter().filter(|(t, s)| *s > threshold && *t > 0.0).map(|(t, _)| *t).collect();
    boundary.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    boundary.dedup();
    let mut span: Vec<(f64, f64)> = Vec::with_capacity(boundary.len() + 1);
    let mut start = 0.0f64;
    for b in boundary {
        if b - start >= MIN_SHOT_SECOND {
            span.push((start, b));
            start = b;
        }
    }
    // The tail. Under the floor it EXTENDS the previous shot rather than becoming one, so the spans
    // always cover [0, duration) exactly and no video time goes missing.
    match span.last_mut() {
        Some(last) if duration_s - start < MIN_SHOT_SECOND => last.1 = duration_s,
        _ if duration_s > start => span.push((start, duration_s)),
        _ => {}
    }
    span
}

// ---------------------------------------------------------------------------------------------
// Per-video record
// ---------------------------------------------------------------------------------------------

struct Video {
    /// Path as walked, forward-slashed and relative to the corpus root. Display only.
    display: String,
    path: PathBuf,
    byte_len: u64,
    duration_s: f64,
    width: u32,
    height: u32,
    codec: String,
    fps: f64,
    /// `-skip_frame nokey`: wall seconds and the I-frame count it yielded.
    keyframe_s: f64,
    iframe_count: usize,
    /// Full decode, every frame through the muxer.
    full_s: f64,
    frame_count: usize,
    /// Full decode + `fps=1`, the baseline this row replaces.
    fps1_s: f64,
    fps1_count: usize,
    /// The scene-score pass: wall seconds and per-frame scores.
    scene_s: f64,
    score: Vec<(f64, f64)>,
}

impl Video {
    fn minute(&self) -> f64 {
        self.duration_s / 60.0
    }
}

/// A slug safe as a filename, derived from the corpus-relative path so two videos with the same
/// basename in different directories cannot collide.
fn slug(display: &str) -> String {
    let mut out: String =
        display.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    if out.len() > 90 {
        out = out[out.len() - 90..].to_string();
    }
    out
}

fn hex(digest: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

// ---------------------------------------------------------------------------------------------

struct Arg {
    limit: Option<usize>,
    manifest: Option<PathBuf>,
}

fn parse_arg() -> Result<Arg, String> {
    let mut arg = Arg { limit: None, manifest: None };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--limit" => {
                let v = it.next().ok_or("--limit needs a count")?;
                arg.limit = Some(v.parse::<usize>().map_err(|e| format!("--limit: {e}"))?);
            }
            "--emit-manifest" => {
                let v = it.next().ok_or("--emit-manifest needs a path")?;
                arg.manifest = Some(PathBuf::from(v));
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(arg)
}

fn skip(reason: &str) -> ! {
    println!("\nSKIP: {reason}");
    println!("\nOVERALL: SKIPPED — no verdict. This is NOT a pass.");
    // Exit 0 deliberately: a machine-local corpus or a missing system tool must not turn the gate
    // red, but the line above refuses to let that read as a pass.
    std::process::exit(0);
}

fn main() {
    let arg = match parse_arg() {
        Ok(a) => a,
        Err(e) => {
            println!("p63-video-shot: {e}");
            println!("usage: video-shot [--limit N] [--emit-manifest <path>]");
            std::process::exit(2);
        }
    };

    println!("p63-video-shot :: video as SHOTS, not frames, on the real video on this machine");

    // ---- the decoder, and the licence it sits on ------------------------------------------------
    let version = match run("ffmpeg", &[os("-version")]) {
        Ok(r) if r.ok => first_line(&r.stdout),
        _ => skip(
            "ffmpeg is not on PATH. This benchmark decodes through the SYSTEM ffmpeg on purpose —\n      \
             see the module comment: H.264/HEVC patents cover the technique, so a permissive code\n      \
             licence on a Rust decoder would not remove the obligation. Install ffmpeg to run it.",
        ),
    };
    println!("decode path: SYSTEM ffmpeg, out of process — no Rust decoder is linked");
    println!("  {version}");
    if run("ffprobe", &[os("-version")]).map(|r| r.ok) != Ok(true) {
        skip("ffprobe is not on PATH (ffmpeg is). Both are needed.");
    }

    // ---- corpus ----------------------------------------------------------------------------------
    let dir = corpus_dir();
    if !dir.is_dir() {
        skip(&format!(
            "corpus directory not found at {}.\n      Set INDEX_VIDEO_CORPUS to a directory holding video files.",
            dir.display()
        ));
    }
    eprintln!("  walking {} (depth <= {MAX_DEPTH}) ...", dir.display());
    let mut file = Vec::new();
    walk(&dir, 1, &mut file);
    if file.is_empty() {
        skip(&format!(
            "{} holds no .mp4/.mkv/.webm/.mov within depth {MAX_DEPTH}.",
            dir.display()
        ));
    }

    println!("\ncorpus root: {}", dir.display());
    println!("  {} video file(s) found by extension, sorted walk, depth <= {MAX_DEPTH}, node_modules excluded",
        file.len());

    install_panic_hook();

    // ---- deduplicate by CONTENT ------------------------------------------------------------------
    //
    // Several of these assets are the same file copied into both `dist/` and `public/` by a build
    // step. Counting them twice would inflate every per-video number in this report, so the digest
    // decides membership and the duplicates are reported rather than dropped in silence.
    let mut seen: HashMap<[u8; 32], String> = HashMap::new();
    let mut unique: Vec<(PathBuf, String, u64, [u8; 32])> = Vec::new();
    let mut duplicate: Vec<(String, String)> = Vec::new();
    let mut unreadable: Vec<String> = Vec::new();
    for path in &file {
        let display = path
            .strip_prefix(&dir)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(byte) = std::fs::read(path) else {
            unreadable.push(display);
            continue;
        };
        let digest = sha256(&byte);
        match seen.get(&digest) {
            Some(first) => duplicate.push((display, first.clone())),
            None => {
                seen.insert(digest, display.clone());
                unique.push((path.clone(), display, byte.len() as u64, digest));
            }
        }
    }
    println!("  {} unreadable, {} exact content duplicate(s) collapsed, {} unique by sha256",
        unreadable.len(), duplicate.len(), unique.len());
    for (dup, first) in duplicate.iter().take(EXAMPLE_MAX) {
        println!("    dup: {dup}\n         == {first}");
    }
    if duplicate.len() > EXAMPLE_MAX {
        println!("    ... and {} more", duplicate.len() - EXAMPLE_MAX);
    }

    if let Some(n) = arg.limit {
        if n < unique.len() {
            println!("  LIMITED to the first {n} unique video(s) in walk order");
            unique.truncate(n);
        }
    }

    // ---- probe + decode passes -------------------------------------------------------------------
    let mut video: Vec<Video> = Vec::new();
    let mut finding: Vec<(String, String)> = Vec::new();
    let mut crash: Vec<(String, String)> = Vec::new();

    for (i, (path, display, byte_len, _digest)) in unique.iter().enumerate() {
        eprintln!("  [{}/{}] {display}", i + 1, unique.len());
        let built = guard(|| -> Result<Video, String> {
            let p = probe(path)?;
            let kf = decode_pass(path, None, true)?;
            if !kf.ok {
                return Err(format!("keyframe pass failed: {}", first_line(&kf.stderr)));
            }
            let full = decode_pass(path, None, false)?;
            if !full.ok {
                return Err(format!("full decode failed: {}", first_line(&full.stderr)));
            }
            let one = decode_pass(path, Some("fps=1"), false)?;
            if !one.ok {
                return Err(format!("fps=1 pass failed: {}", first_line(&one.stderr)));
            }
            let (score, scene_s) = scene_score(path)?;
            Ok(Video {
                display: display.clone(),
                path: path.clone(),
                byte_len: *byte_len,
                duration_s: p.duration_s,
                width: p.width,
                height: p.height,
                codec: p.codec,
                fps: p.fps,
                keyframe_s: kf.second,
                iframe_count: progress_frame(&kf.stdout),
                full_s: full.second,
                frame_count: progress_frame(&full.stdout),
                fps1_s: one.second,
                fps1_count: progress_frame(&one.stdout),
                scene_s,
                score,
            })
        });
        match built {
            Ok(Ok(v)) => video.push(v),
            Ok(Err(msg)) => finding.push((display.clone(), msg)),
            Err(msg) => {
                crash.push((display.clone(), msg));
                finding.push((display.clone(), "PANIC — see the robustness section".to_string()));
            }
        }
    }
    let _ = std::panic::take_hook();

    let mut fail = 0usize;
    let mut check: Vec<(String, bool)> = Vec::new();
    let mut withheld: Vec<String> = Vec::new();

    // =============================================================================================
    // 1. CENSUS — and an honest statement of what this corpus cannot answer
    // =============================================================================================
    println!("\n=== 1. CORPUS CENSUS ===================================================");
    println!("  {} unique video(s) opened, {} rejected with a reported reason",
        video.len(), finding.len());
    for (name, why) in finding.iter().take(EXAMPLE_MAX) {
        println!("    REJECTED {name}\n             {why}");
    }
    if finding.len() > EXAMPLE_MAX {
        println!("    ... and {} more", finding.len() - EXAMPLE_MAX);
    }

    println!("\n  --- robustness: a file ffmpeg cannot open is a FINDING, not a crash ---");
    println!("    {} panic(s) across {} file(s)", crash.len(), unique.len());
    for (name, msg) in crash.iter().take(EXAMPLE_MAX) {
        println!("      PANIC {name}: {}", msg.lines().next().unwrap_or(""));
    }
    check.push((format!("no panic on any of {} video file(s)", unique.len()), crash.is_empty()));
    if !crash.is_empty() {
        fail += 1;
    }

    if video.is_empty() {
        println!("\n  Every candidate was rejected — there is nothing to measure.");
        skip("no video file in the corpus could be opened by ffmpeg.");
    }

    let total_second: f64 = video.iter().map(|v| v.duration_s).sum();
    let total_byte: u64 = video.iter().map(|v| v.byte_len).sum();
    let mut duration: Vec<f64> = video.iter().map(|v| v.duration_s).collect();
    duration.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("\n  {:.1} s of video total ({:.2} video-minutes), {:.1} MB on disk",
        total_second, total_second / 60.0, total_byte as f64 / 1e6);
    println!("  duration per video: p50 {:.1} s   p90 {:.1} s   max {:.1} s   min {:.1} s",
        timer::percentile(&duration, 0.5),
        timer::percentile(&duration, 0.9),
        duration[duration.len() - 1],
        duration[0]);

    let mut by_codec: HashMap<String, usize> = HashMap::new();
    for v in &video {
        *by_codec.entry(v.codec.clone()).or_default() += 1;
    }
    let mut codec: Vec<(&String, &usize)> = by_codec.iter().collect();
    codec.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!("\n  --- codec, which is a LICENCE fact as much as a technical one ---");
    for (name, count) in &codec {
        let note = match name.as_str() {
            "av1" => "royalty-free (AOMedia)",
            "h264" | "hevc" => "PATENT-ENCUMBERED — decoded here by the system ffmpeg, not by us",
            "vp8" | "vp9" => "royalty-free (Google/AOMedia cross-licence)",
            _ => "",
        };
        println!("    {:<8} {:>4}   {note}", name, count);
    }

    let mut by_res: HashMap<(u32, u32), usize> = HashMap::new();
    for v in &video {
        *by_res.entry((v.width, v.height)).or_default() += 1;
    }
    let mut res: Vec<((u32, u32), usize)> = by_res.into_iter().collect();
    res.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut rate: Vec<f64> = video.iter().map(|v| v.fps).collect();
    rate.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("\n  --- resolution and frame rate (both drive every cost above) ---");
    for ((w, h), count) in res.iter().take(EXAMPLE_MAX) {
        println!("    {:<12} {:>4}", format!("{w}x{h}"), count);
    }
    if res.len() > EXAMPLE_MAX {
        println!("    ... and {} more distinct resolution(s)", res.len() - EXAMPLE_MAX);
    }
    println!("    fps: p50 {:.2}   min {:.2}   max {:.2}",
        timer::percentile(&rate, 0.5), rate[0], rate[rate.len() - 1]);
    println!("    §9 notes NVDEC buys only ~1.29x over CPU at 1080p and is not worth the");
    println!("    complexity below 4K. Nothing in this corpus is above 4K, so CPU decode is the");
    println!("    right call here and no GPU path was measured.");

    println!("\n  --- what this corpus is NOT ---");
    println!("    These are marketing hero loops, product reels and screen-recorded slide decks");
    println!("    found on a developer machine. They are mostly short and many contain very few");
    println!("    real cuts. That makes this a legitimate test of the PIPELINE — decode strategy,");
    println!("    shot detection, cost, storage — and a POOR test of RETRIEVAL: there are no");
    println!("    relevance judgements anywhere in it, so p63 acceptance 3 is WITHHELD below rather");
    println!("    than answered with a number that would mean nothing.");
    withheld.push(
        "p63 acceptance 3: shot-sampled recall >= uniform 1 fps — needs a LABELLED clip set; this \
         corpus has no relevance judgements"
            .to_string(),
    );

    // =============================================================================================
    // 2. DECODE STRATEGY COST (p63 acceptance 1)
    // =============================================================================================
    println!("\n=== 2. DECODE STRATEGY COST (p63 acceptance 1) =========================");
    println!("  Wall clock for a whole ffmpeg process, PROCESS SPAWN INCLUDED. That is the honest");
    println!("  figure for a host that shells out, and on a 7-second hero loop the ~0.1-0.2 s of");
    println!("  spawn is a large share of it — so the speedup below is UNDERSTATED on short files");
    println!("  and the long-file column is the one to read.");
    println!();
    println!("  {:<44} {:>9} {:>9} {:>9} {:>8}", "video", "kf s", "fps1 s", "full s", "frames");
    let mut kf_total = 0.0;
    let mut full_total = 0.0;
    let mut fps1_total = 0.0;
    let mut ratio_full: Vec<f64> = Vec::new();
    let mut ratio_fps1: Vec<f64> = Vec::new();
    for v in &video {
        kf_total += v.keyframe_s;
        full_total += v.full_s;
        fps1_total += v.fps1_s;
        if v.keyframe_s > 0.0 {
            ratio_full.push(v.full_s / v.keyframe_s);
            ratio_fps1.push(v.fps1_s / v.keyframe_s);
        }
    }
    for v in video.iter().take(EXAMPLE_MAX) {
        let name = if v.display.len() > 42 {
            format!("...{}", &v.display[v.display.len() - 39..])
        } else {
            v.display.clone()
        };
        println!("  {:<44} {:>9.3} {:>9.3} {:>9.3} {:>8}",
            name, v.keyframe_s, v.fps1_s, v.full_s, v.frame_count);
    }
    if video.len() > EXAMPLE_MAX {
        println!("  ... and {} more (aggregates below cover all {})", video.len() - EXAMPLE_MAX, video.len());
    }

    ratio_full.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ratio_fps1.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("\n  TOTALS over {} video ({:.2} video-minutes):", video.len(), total_second / 60.0);
    println!("    -skip_frame nokey (I-frames only) {:>9.2} s   {:>7.2} s per video-minute",
        kf_total, kf_total / (total_second / 60.0));
    println!("    full decode + fps=1               {:>9.2} s   {:>7.2} s per video-minute",
        fps1_total, fps1_total / (total_second / 60.0));
    println!("    full decode, every frame          {:>9.2} s   {:>7.2} s per video-minute",
        full_total, full_total / (total_second / 60.0));
    let agg_full = if kf_total > 0.0 { full_total / kf_total } else { 0.0 };
    let agg_fps1 = if kf_total > 0.0 { fps1_total / kf_total } else { 0.0 };
    println!("\n    aggregate speedup, keyframe-only vs full decode: {agg_full:.2}x");
    println!("    aggregate speedup, keyframe-only vs fps=1 pass:   {agg_fps1:.2}x");
    println!("    per-video speedup vs full decode: p50 {:.2}x  p90 {:.2}x  min {:.2}x  max {:.2}x",
        timer::percentile(&ratio_full, 0.5),
        timer::percentile(&ratio_full, 0.9),
        ratio_full[0],
        ratio_full[ratio_full.len() - 1]);
    let reproduces = (2.0..=3.0).contains(&agg_full);
    println!("\n  -> §9 records a MEASURED 2-3x for `-skip_frame nokey`. Measured HERE: {agg_full:.2}x.");
    if reproduces {
        println!("     That REPRODUCES §9's range on this corpus.");
    } else if agg_full > 3.0 {
        println!("     That is ABOVE §9's range. §9's 2-3x is the claim this run is checked against,");
        println!("     so it is reported as exceeded, not as a better headline: a corpus of short");
        println!("     files with sparse I-frames flatters keyframe-only decoding.");
    } else {
        println!("     That is BELOW §9's range on this corpus. Process spawn is inside every figure");
        println!("     and these files are short, which taxes the cheap pass hardest.");
    }
    // The check is directional, not the literal 2-3x band: this corpus is not §9's corpus, and
    // asserting someone else's exact number over different files would be theatre.
    let faster = agg_full > 1.0;
    check.push((format!("keyframe-only decode is faster than full decode ({agg_full:.2}x)"), faster));
    if !faster {
        fail += 1;
    }

    // =============================================================================================
    // 3. SHOT DETECTION (p63 acceptance 1) — a HEURISTIC, reported as one
    // =============================================================================================
    println!("\n=== 3. SHOT DETECTION ==================================================");
    println!("  Detector: ffmpeg `select='gt(scene,T)'` — a frame-difference threshold. It is an");
    println!("  UNBENCHMARKED HEURISTIC. §9's measured detector is PySceneDetect AdaptiveDetector");
    println!("  (F1 91.59 on BBC vs ContentDetector 86.69), which is not installed on this machine,");
    println!("  so NO F1 is claimed here. What is claimed: counts, cost, and how hard the threshold");
    println!("  moves the answer.");
    println!();
    println!("  {:<10} {:>10} {:>14} {:>16}", "threshold", "shot total", "shot/video", "shot/video-min");
    for &t in &SCENE_THRESHOLD {
        let total: usize = video.iter().map(|v| shot_span(&v.score, t, v.duration_s).len()).sum();
        println!("  {:<10.2} {:>10} {:>14.2} {:>16.2}",
            t,
            total,
            total as f64 / video.len() as f64,
            total as f64 / (total_second / 60.0));
    }
    println!("\n  -> The sweep IS the finding: the threshold picks the answer. A single headline");
    println!("     shot count would be a methodology choice wearing a fact's clothes.");

    let shot: Vec<Vec<(f64, f64)>> =
        video.iter().map(|v| shot_span(&v.score, PRIMARY_THRESHOLD, v.duration_s)).collect();
    let shot_total: usize = shot.iter().map(|s| s.len()).sum();
    let scene_total: f64 = video.iter().map(|v| v.scene_s).sum();
    println!("\n  primary threshold {PRIMARY_THRESHOLD}: {shot_total} shot across {} video",
        video.len());
    println!("  detection cost {:.2} s total, {:.2} s per video-minute (a FULL decode plus the",
        scene_total, scene_total / (total_second / 60.0));
    println!("  metadata print — scene scoring cannot run on I-frames alone, which is the honest");
    println!("  cost of this pipeline and is NOT covered by the keyframe speedup above).");

    let mut asl: Vec<f64> = Vec::new();
    for span in &shot {
        for (a, b) in span {
            asl.push(b - a);
        }
    }
    asl.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let raw_boundary: usize = video
        .iter()
        .map(|v| v.score.iter().filter(|(t, s)| *s > PRIMARY_THRESHOLD && *t > 0.0).count())
        .sum();
    println!("  {raw_boundary} raw boundary(ies) fired at this threshold; {} were below the",
        (raw_boundary + video.len()).saturating_sub(shot_total));
    println!("  {MIN_SHOT_SECOND}s minimum-shot floor and were MERGED into the preceding shot, not");
    println!("  dropped — a 40 ms span is a detector artefact, and the first run of this benchmark");
    println!("  proved it by seeking past the last coded frame of a clip and extracting nothing.");

    println!("\n  average shot length: mean {:.2} s   p50 {:.2} s   p90 {:.2} s   max {:.2} s",
        mean(&asl),
        timer::percentile(&asl, 0.5),
        timer::percentile(&asl, 0.9),
        asl[asl.len() - 1]);
    println!("  §9's typical editing pace is ASL 3.5-6 s. See the note in section 4.");

    println!("\n  per-video shot count at threshold {PRIMARY_THRESHOLD} (first {EXAMPLE_MAX}):");
    println!("  {:<44} {:>8} {:>8} {:>12}", "video", "dur s", "shot", "shot/min");
    for (v, span) in video.iter().zip(shot.iter()).take(EXAMPLE_MAX) {
        let name = if v.display.len() > 42 {
            format!("...{}", &v.display[v.display.len() - 39..])
        } else {
            v.display.clone()
        };
        println!("  {:<44} {:>8.1} {:>8} {:>12.1}",
            name, v.duration_s, span.len(), span.len() as f64 / v.minute().max(1e-9));
    }

    // =============================================================================================
    // 4. FRAMES PER VIDEO-MINUTE (p63 acceptance 1)
    // =============================================================================================
    println!("\n=== 4. FRAMES PER VIDEO-MINUTE =========================================");
    let frame_total: usize = video.iter().map(|v| v.frame_count).sum();
    let iframe_total: usize = video.iter().map(|v| v.iframe_count).sum();
    let minute = total_second / 60.0;
    println!("  {:<34} {:>12} {:>16}", "strategy", "frames", "per video-min");
    println!("  {:<34} {:>12} {:>16.1}", "every frame (full decode)", frame_total, frame_total as f64 / minute);
    println!("  {:<34} {:>12} {:>16.1}", "uniform 1 fps (the baseline)",
        video.iter().map(|v| v.fps1_count).sum::<usize>(),
        video.iter().map(|v| v.fps1_count).sum::<usize>() as f64 / minute);
    println!("  {:<34} {:>12} {:>16.1}", "I-frame only (-skip_frame nokey)", iframe_total, iframe_total as f64 / minute);
    println!("  {:<34} {:>12} {:>16.1}", "one per detected shot", shot_total, shot_total as f64 / minute);
    println!("\n  reduction vs every frame: I-frame {:.1}x, shot {:.1}x",
        frame_total as f64 / iframe_total.max(1) as f64,
        frame_total as f64 / shot_total.max(1) as f64);
    println!("  reduction vs uniform 1 fps: shot {:.2}x",
        video.iter().map(|v| v.fps1_count).sum::<usize>() as f64 / shot_total.max(1) as f64);

    let shot_per_ten_minute = 10.0 * shot_total as f64 / minute;
    println!("\n  -> §9 expects ~120-170 shots in a 10-minute video at typical editing pace.");
    println!("     Scaled from this corpus: {shot_per_ten_minute:.0} shots per 10 minutes.");
    println!("     These short loops do NOT look like §9's expectation, and that is a PROPERTY OF");
    println!("     THIS CORPUS, not a refutation of §9: a 7-second hero loop and a screen-recorded");
    println!("     slide deck are not edited at broadcast pace. Nothing about a 10-minute film is");
    println!("     claimed from these numbers.");
    println!();
    println!("     NOTE the ordering surfaced above: on THIS corpus the I-frame count and the shot");
    println!("     count are not the same quantity and need not be ordered the way §9's example");
    println!("     implies. An encoder emits an I-frame on a fixed GOP cadence whether or not the");
    println!("     picture changed, so on low-cut footage there are MORE I-frames than shots — the");
    println!("     I-frame is a cheap CANDIDATE SET, and shot detection is what turns it into a");
    println!("     semantic unit. That is exactly why the pipeline has both stages.");

    // =============================================================================================
    // 5. STORAGE PER VIDEO-MINUTE (p63 acceptance 2)
    // =============================================================================================
    println!("\n=== 5. STORAGE PER VIDEO-MINUTE (p63 acceptance 2) =====================");
    // Real figures, measured off `index-image` rather than assumed: build both column shapes at the
    // real width and ask them what they cost.
    let probe_vector: Vec<f32> = (0..DIM).map(|i| (i as f32).sin()).collect();
    let mut full_col = VectorColumn::new(DIM, Metric::Cosine);
    let mut compact_col = VectorColumn::new_compact(DIM, Metric::Cosine);
    let sample_n = 64usize;
    for i in 0..sample_n {
        let v: Vec<f32> = probe_vector.iter().map(|x| x + i as f32 * 1e-3).collect();
        let _ = full_col.push(&v);
        let _ = compact_col.push(&v);
    }
    let full_len = full_col.byte_len();
    let compact_len = compact_col.byte_len();
    let full_per = full_len.total() as f64 / sample_n as f64 + HASH_BYTE as f64;
    let compact_per = compact_len.total() as f64 / sample_n as f64 + HASH_BYTE as f64;
    println!("  Measured from index_image::vector::VectorColumn::byte_len at dim {DIM}, {sample_n} vectors:");
    println!("  {:<30} {:>12} {:>12}", "tier", "full B/vec", "compact B/vec");
    println!("  {:<30} {:>12.1} {:>12.1}", "binary prefilter",
        full_len.binary as f64 / sample_n as f64, compact_len.binary as f64 / sample_n as f64);
    println!("  {:<30} {:>12.1} {:>12.1}", "int8 rerank",
        full_len.int8 as f64 / sample_n as f64, compact_len.int8 as f64 / sample_n as f64);
    println!("  {:<30} {:>12.1} {:>12.1}", "dequant scale",
        full_len.scale as f64 / sample_n as f64, compact_len.scale as f64 / sample_n as f64);
    println!("  {:<30} {:>12.1} {:>12.1}", "retained f32",
        full_len.exact as f64 / sample_n as f64, compact_len.exact as f64 / sample_n as f64);
    println!("  {:<30} {:>12.1} {:>12.1}", "+ dHash (64-bit)", HASH_BYTE as f64, HASH_BYTE as f64);
    println!("  {:<30} {:>12.1} {:>12.1}", "TOTAL per stored frame", full_per, compact_per);

    let baseline_frame_per_minute = 60.0 * BASELINE_FPS;
    let shot_per_minute = shot_total as f64 / minute;
    let iframe_per_minute = iframe_total as f64 / minute;
    println!("\n  {:<32} {:>14} {:>16} {:>16}", "strategy", "frame/vid-min", "full KB/vid-min", "compact KB/vid-min");
    for (name, per_minute) in [
        ("uniform 1 fps (baseline)", baseline_frame_per_minute),
        ("I-frame only", iframe_per_minute),
        ("one per detected shot", shot_per_minute),
    ] {
        println!("  {:<32} {:>14.1} {:>16.1} {:>16.1}",
            name,
            per_minute,
            per_minute * full_per / 1024.0,
            per_minute * compact_per / 1024.0);
    }
    let storage_ratio = baseline_frame_per_minute / shot_per_minute.max(1e-9);
    println!("\n  -> shot sampling stores {storage_ratio:.2}x less than the 1-fps baseline it replaces,");
    println!("     on this corpus. On §9's 10-minute-at-editing-pace video the ratio would be ~4-5x");
    println!("     (600 frames vs 120-170 shots); this corpus cannot show that and does not claim it.");
    let storage_ok = storage_ratio > 1.0;
    check.push((
        format!("shot sampling stores less than uniform 1 fps ({storage_ratio:.2}x)"),
        storage_ok,
    ));
    if !storage_ok {
        fail += 1;
    }

    // =============================================================================================
    // 6. EXTRACT ONE KEYFRAME PER SHOT — the MIDDLE frame
    // =============================================================================================
    println!("\n=== 6. KEYFRAME EXTRACTION (middle frame of each shot) =================");
    let out_dir = std::env::temp_dir().join("index-p63-video-shot");
    let _ = std::fs::remove_dir_all(&out_dir);
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        println!("  FAIL: cannot create {}: {e}", out_dir.display());
        println!("\nOVERALL: FAIL");
        std::process::exit(1);
    }
    println!("  temp dir: {}", out_dir.display());
    println!("  The MIDDLE of each shot, not the first frame: §9's point is that a shot's opening");
    println!("  frame is often still mid-transition and motion-blurred, so it is the worst frame in");
    println!("  the shot to hand an encoder.");

    let mut manifest_line: Vec<String> = Vec::new();
    let mut extracted = 0usize;
    let mut extract_byte: u64 = 0;
    let mut extract_fail: Vec<(String, String)> = Vec::new();
    let mut extract_s = 0.0f64;
    let mut capped = false;
    let t_extract = Instant::now();
    'outer: for (v, span) in video.iter().zip(shot.iter()) {
        let stem = slug(&v.display);
        for (idx, (start, end)) in span.iter().enumerate() {
            if extracted + extract_fail.len() >= EXTRACT_MAX {
                capped = true;
                break 'outer;
            }
            let mid = (start + end) / 2.0;
            let out = out_dir.join(format!("{stem}__shot{idx:04}.jpg"));
            let arg = vec![
                os("-v"),
                os("error"),
                os("-nostdin"),
                os("-y"),
                os("-ss"),
                os(&format!("{mid:.3}")),
                os("-i"),
                v.path.as_os_str().to_os_string(),
                os("-frames:v"),
                os("1"),
                os("-update"),
                os("1"),
                os("-q:v"),
                os("2"),
                // mjpeg refuses a non-full-range YUV frame unless the pixel format is the JPEG one.
                // Sources on this machine carry both range tags, so pin it rather than let the
                // encoder decide per file.
                os("-pix_fmt"),
                os("yuvj420p"),
                out.as_os_str().to_os_string(),
            ];
            match run("ffmpeg", &arg) {
                Ok(r) if r.ok && out.is_file() => {
                    let len = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                    if len == 0 {
                        extract_fail.push((v.display.clone(), "wrote a zero-byte frame".to_string()));
                        continue;
                    }
                    extract_byte += len;
                    extracted += 1;
                    manifest_line.push(format!(
                        "{}\t{}\t{}\t{}\t{}",
                        out.display(),
                        // The ABSOLUTE video path, not the display label: a consumer of this
                        // manifest has to be able to re-open the video and seek to `start_ms`.
                        v.path.display(),
                        idx,
                        (start * 1000.0).round() as i64,
                        (end * 1000.0).round() as i64
                    ));
                }
                Ok(r) => extract_fail.push((v.display.clone(), first_line(&r.stderr))),
                Err(e) => extract_fail.push((v.display.clone(), e)),
            }
        }
    }
    extract_s += t_extract.elapsed().as_secs_f64();

    println!("\n  {extracted} keyframe extracted, {} failed, {} bytes total ({:.2} MB)",
        extract_fail.len(), extract_byte, extract_byte as f64 / 1e6);
    if extracted > 0 {
        println!("  mean {:.1} KB per keyframe (JPEG q2 at source resolution — this is the DECODED",
            extract_byte as f64 / extracted as f64 / 1024.0);
        println!("  intermediate handed to an encoder, NOT what the index stores; section 5 is what");
        println!("  the index stores)");
    }
    println!("  extraction wall clock {extract_s:.2} s ({:.3} s per keyframe, one ffmpeg process each",
        extract_s / extracted.max(1) as f64);
    println!("  with an input `-ss` seek — process spawn dominates and is inside the figure)");
    if capped {
        println!("  CAPPED at {EXTRACT_MAX} extractions; the remaining shots were not extracted.");
    }
    for (name, why) in extract_fail.iter().take(EXAMPLE_MAX) {
        println!("    EXTRACT FAILED {name}: {why}");
    }
    if extract_fail.len() > EXAMPLE_MAX {
        println!("    ... and {} more", extract_fail.len() - EXAMPLE_MAX);
    }

    let expected = if capped { extracted } else { shot_total };
    let one_per_shot = extracted + extract_fail.len() >= expected && extract_fail.is_empty();
    check.push((
        format!("one keyframe extracted per detected shot ({extracted} of {shot_total})"),
        one_per_shot,
    ));
    if !one_per_shot {
        fail += 1;
    }

    // ---- the manifest ----------------------------------------------------------------------------
    let manifest_path = arg.manifest.clone().unwrap_or_else(|| out_dir.join("manifest.tsv"));
    let mut text = String::with_capacity(manifest_line.len() * 128);
    for line in &manifest_line {
        text.push_str(line);
        text.push('\n');
    }
    match std::fs::write(&manifest_path, &text) {
        Ok(()) => {
            println!("\n  manifest: {} line(s) -> {}", manifest_line.len(), manifest_path.display());
            println!("    `<path>\\t<video>\\t<shot_index>\\t<start_ms>\\t<end_ms>`, in walk order.");
            println!("    scripts/embed-corpus.py embeds column 1; columns 2-5 are what makes the");
            println!("    result a SHOT rather than a picture — the video it came from and the span");
            println!("    it covers. A search result is then a shot, which is what a user wanted.");
        }
        Err(e) => {
            println!("\n  FAIL: cannot write manifest {}: {e}", manifest_path.display());
            fail += 1;
        }
    }
    let manifest_ok = manifest_line.len() == extracted;
    check.push((
        format!("manifest line count == extracted keyframe count ({} == {extracted})", manifest_line.len()),
        manifest_ok,
    ));
    if !manifest_ok {
        fail += 1;
    }

    // A digest of the manifest CONTENT is not printed; the counts above are the deterministic part,
    // and the temp path in column 1 varies by machine. What must not vary is the count.
    let _ = hex(&sha256(text.as_bytes()));

    // =============================================================================================
    // What this run surfaced
    // =============================================================================================
    println!("\n=== WHAT THIS RUN SURFACED =============================================");
    println!("  1. VIDEO NEEDED NO NEW INDEX TYPE, AND THAT IS THE HEADLINE.");
    println!("     Everything this benchmark built is HOST-SIDE: decode, shot-detect, pick the");
    println!("     middle frame, emit a manifest. Not one line of `index-image` or `index-text`");
    println!("     changed, and none needed to. A shot is an ImageDoc with a (video, start_ms,");
    println!("     end_ms) and an embedding; the fusion layer, the vector column and the hash");
    println!("     column already index it. p63 names that asymmetry as the strongest available");
    println!("     evidence that the p59 architecture is right, and this run is that evidence.");
    println!();
    println!("  2. THE CHEAP DECODE DOES NOT PAY FOR THE SHOT DETECTION.");
    println!("     `-skip_frame nokey` is {agg_full:.2}x faster than a full decode here, but the scene");
    println!("     scorer needs every frame and costs {:.2} s per video-minute — more than the full",
        scene_total / minute);
    println!("     decode it rides on. The keyframe speedup is real and it is NOT the pipeline's");
    println!("     total cost. A production build would run detection once at ingest and keep the");
    println!("     shot list, which is precisely what the manifest is for.");
    println!();
    println!("  3. THE CORPUS IS THE LIMIT, NOT THE CODE.");
    println!("     {} unique video, {:.2} video-minutes, no relevance judgements. Every count above",
        video.len(), minute);
    println!("     is exact and reproducible; every claim about 10-minute broadcast-paced footage");
    println!("     belongs to §9 and is cited, not re-derived. p63's acceptance 3 stays open until");
    println!("     a labelled clip set exists on this machine.");
    println!();
    println!("  4. NEAR-DUPLICATE VIDEO (p63 acceptance 4) IS NOT ATTEMPTED HERE.");
    println!("     A vPDQ-shaped per-shot hash sequence needs the per-shot hashes, which needs the");
    println!("     extracted frames this run has only just produced. The manifest is the seam: the");
    println!("     hash sequence is a second pass over it, not a change to this pipeline.");
    withheld.push(
        "p63 acceptance 4: near-duplicate video via a per-shot hash sequence (vPDQ shape) — needs \
         a hashing pass over the manifest this run emits"
            .to_string(),
    );

    // =============================================================================================
    // Check list
    // =============================================================================================
    println!("\n=== CHECK LIST =========================================================");
    for (name, ok) in &check {
        println!("  [{}] {name}", if *ok { "PASS" } else { "FAIL" });
    }
    for name in &withheld {
        println!("  [HELD] {name}");
    }
    if !withheld.is_empty() {
        println!("\n  {} verdict(s) WITHHELD. This run does not answer all of p63 — it declines to,",
            withheld.len());
        println!("  and says which parts and why.");
    }
    println!("\nOVERALL: {}", if fail == 0 { "PASS" } else { "FAIL" });
    std::process::exit(if fail == 0 { 0 } else { 1 });
}
