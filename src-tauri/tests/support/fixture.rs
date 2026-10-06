use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub const CLIP_SECS: u32 = 9;
// Measured with the pinned ffmpeg (aarch64-apple-darwin): 9 video, 10 audio .ts segments.

pub struct Fixture {
    pub dir: PathBuf,
    pub video_segments: usize,
    pub audio_segments: usize,
}

pub fn ensure(ffmpeg: &Path) -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| synthesize(ffmpeg))
}

fn synthesize(ffmpeg: &Path) -> Fixture {
    // Per-process dir: one `cargo test --features test-utils` runs two test
    // binaries; a fixed path would let them tear each other's fixture.
    let dir = std::env::temp_dir().join(format!("video-fetch-e2e-fixture-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create fixture dir");

    run(
        ffmpeg,
        &dir,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=15",
            "-t",
            &CLIP_SECS.to_string(),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-b:v",
            "320k",
            // One keyframe per second: without this the default interval collapses
            // the clip into a single HLS segment (probe finding).
            "-g",
            "15",
            "-keyint_min",
            "15",
            "-sc_threshold",
            "0",
            "-f",
            "hls",
            "-hls_time",
            "1",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
            "v%d.ts",
            "v.m3u8",
        ],
    );
    run(
        ffmpeg,
        &dir,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=9",
            "-c:a",
            "aac",
            "-b:a",
            "64k",
            "-f",
            "hls",
            "-hls_time",
            "1",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
            "a%d.ts",
            "a.m3u8",
        ],
    );
    // Plain two-variant master: yt-dlp derives numeric format ids from
    // BANDWIDTH (production sources are numeric itags; EXT-X-MEDIA would yield
    // names like faud-audio that has_fragment_infix cannot match).
    std::fs::write(
        dir.join("master.m3u8"),
        "#EXTM3U\n#EXT-X-VERSION:3\n\
         #EXT-X-STREAM-INF:BANDWIDTH=256000,RESOLUTION=320x240,CODECS=\"avc1.42c01e\"\nv.m3u8\n\
         #EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS=\"mp4a.40.2\"\na.m3u8\n",
    )
    .unwrap();

    Fixture {
        video_segments: count_segments(&dir, "v"),
        audio_segments: count_segments(&dir, "a"),
        dir,
    }
}

fn run(ffmpeg: &Path, dir: &Path, args: &[&str]) {
    let out = Command::new(ffmpeg)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn count_segments(dir: &Path, prefix: &str) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(prefix) && n.ends_with(".ts")
        })
        .count()
}
