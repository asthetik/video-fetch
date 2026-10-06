mod support;

use std::time::Duration;

use video_fetch_lib::testing::JobStatus;

/// Value: protects=the keyframe fix (-g) and the fixture's segmentability;
/// fails_when=-g/-keyint_min/-sc_threshold removed (video collapses to one
/// segment) or the master manifest stops mapping to numeric-format variants;
/// why_new=the probe proved the default keyframe interval produced a single
/// 6s video segment; seam=none
#[tokio::test]
async fn fixture_synthesizes_a_multi_segment_hls_set() {
    let ffmpeg = support::harness::find_sidecar("ffmpeg");
    let fx = support::fixture::ensure(&ffmpeg);
    assert!(
        fx.video_segments >= 6,
        "video segments: {}",
        fx.video_segments
    );
    assert!(
        fx.audio_segments >= 6,
        "audio segments: {}",
        fx.audio_segments
    );
    for name in ["master.m3u8", "v.m3u8", "a.m3u8", "v0.ts", "a0.ts"] {
        assert!(fx.dir.join(name).is_file(), "missing fixture file {name}");
    }
    // Naming shape: numeric format ids, same as production (see the spec).
    let master = std::fs::read_to_string(fx.dir.join("master.m3u8")).unwrap();
    assert!(master.contains("BANDWIDTH="));
    assert!(
        !master.contains("EXT-X-MEDIA"),
        "fixture must map to numeric format ids"
    );
}

/// Value: protects=server request classification and the gate; fails_when=
/// fragments counted as playlists (zero-redownload asserts break) or the gate
/// releases early; why_new=no local server exists yet; seam=none
#[tokio::test]
async fn server_classifies_requests_and_holds_the_gate() {
    let ffmpeg = support::harness::find_sidecar("ffmpeg");
    let fx = support::fixture::ensure(&ffmpeg);
    let srv = support::server::TestServer::start(&fx.dir);

    // Plain GET: playlists and fragments land in their own buckets
    let body = support::server::get_text(&srv.url(1, "master.m3u8"));
    assert!(body.contains("#EXTM3U"));
    let _ = support::server::get_text(&srv.url(1, "v0.ts"));
    let _ = support::server::get_text(&srv.url(1, "a0.ts"));
    let c = srv.counts(1);
    assert_eq!(
        (c.playlists, c.video_fragments, c.audio_fragments),
        (1, 1, 1)
    );

    // Gate: the next fragment request is held until explicitly released
    srv.arm_gate_on_next_fragment(1);
    let url = srv.url(1, "v1.ts");
    let fetch = tokio::task::spawn_blocking(move || support::server::get_text(&url));
    assert!(srv.wait_gate_engaged(Duration::from_secs(10)).await);
    assert!(!fetch.is_finished(), "gated request must still be pending");
    srv.release_gate();
    let _ = fetch.await.unwrap();
    assert_eq!(srv.counts(1).video_fragments, 2);

    // Ordinal gate: video 2's first fragment passes, the second is held
    srv.arm_gate_on_nth_fragment(2, 2);
    let _ = support::server::get_text(&srv.url(2, "v0.ts"));
    let url = srv.url(2, "v1.ts");
    let fetch = tokio::task::spawn_blocking(move || support::server::get_text(&url));
    assert!(srv.wait_gate_engaged(Duration::from_secs(10)).await);
    assert!(!fetch.is_finished(), "gated request must still be pending");
    srv.release_gate();
    let _ = fetch.await.unwrap();
    assert_eq!(srv.counts(2).video_fragments, 2);
}

/// Value: protects=HEAD/Range tolerance (yt-dlp's resume probing may send
/// either); fails_when=HEAD answered with a body or Range requests answered 404;
/// why_new=unverified assumption from the reviews; seam=none
#[tokio::test]
async fn server_tolerates_head_and_range() {
    let ffmpeg = support::harness::find_sidecar("ffmpeg");
    let fx = support::fixture::ensure(&ffmpeg);
    let srv = support::server::TestServer::start(&fx.dir);
    let head = support::server::request(&srv.url(1, "a0.ts"), "HEAD", &[]);
    assert!(head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.0 200"));
    let ranged = support::server::request(&srv.url(1, "a0.ts"), "GET", &[("Range", "bytes=0-9")]);
    assert!(ranged.contains(" 200 ") || ranged.contains(" 206 "));
}

/// Value: protects=the whole download→merge→deliver pipeline over the real
/// sidecars plus the production fragment naming shape (.f<digits>.<ext>,
/// has_fragment_infix); fails_when=the pinned pair stops merging, the
/// delivered file loses a stream, or the engine starts naming fragments in a
/// shape the unmerged-stream guard cannot match; why_new=no test drives a real
/// yt-dlp/ffmpeg pair; seam=none. Measured (aarch64-apple-darwin, pinned pair):
/// the merge lands as `clip-1.mp4`; the harness globs `clip-1.*` instead of
/// hardcoding the container.
#[tokio::test]
async fn download_completes_and_delivers_a_merged_file() {
    let h = support::harness::Harness::new(1);
    let job = h.enqueue(1);

    // Hold the first fragment so the work dir exists mid-flight: this is the
    // only moment fragments are observable before delivery.
    h.server.arm_gate_on_next_fragment(1);
    assert!(h.server.wait_gate_engaged(Duration::from_secs(30)).await);
    support::oracles::assert_fragment_naming(&h.work_dir_of(&job.id));
    h.server.release_gate();

    h.wait_status(&job.id, JobStatus::Done, Duration::from_secs(60))
        .await;

    let c = h.server.counts(1);
    let fx = support::fixture::ensure(&h.ffmpeg);
    assert_eq!(c.video_fragments, fx.video_segments);
    assert_eq!(c.audio_fragments, fx.audio_segments);

    let delivered = h.delivered_path(1);
    support::oracles::assert_delivered_merged(&h.ffmpeg, &delivered);

    // The work dir is cleaned on completion (gone or empty)
    support::oracles::assert_no_stray_media_files(&h.work_dir_of(&job.id));
}

/// Value: protects=pause keeps fragments and resume continues from the
/// breakpoint (per-stream request accounting, playlists excluded, tolerance
/// +2 for the interrupted fragment's fail+retry); fails_when=the resume
/// delivers nothing merged, the pause drops the work dir, or per-stream
/// requests exceed segments+2; why_new=the spike verified this only by hand;
/// seam=none
#[tokio::test]
async fn pause_resume_finishes_without_redownloading() {
    let h = support::harness::Harness::new(1);
    let job = h.enqueue(1);
    h.server.arm_gate_on_next_fragment(1);
    assert!(h.server.wait_gate_engaged(Duration::from_secs(30)).await);

    h.manager.pause(&job.id).unwrap();
    h.wait_status(&job.id, JobStatus::Paused, Duration::from_secs(30))
        .await;
    h.server.release_gate();

    // Pause keeps the work dir (fragments / `.part`)
    let wd = h.work_dir_of(&job.id);
    assert!(wd.is_dir(), "pause must keep the work dir");

    h.manager.resume(&job.id).unwrap();
    h.wait_status(&job.id, JobStatus::Done, Duration::from_secs(60))
        .await;

    let c = h.server.counts(1);
    let fx = support::fixture::ensure(&h.ffmpeg);
    assert!(
        c.video_fragments <= fx.video_segments + 2,
        "video re-downloaded: {c:?}"
    );
    assert!(
        c.audio_fragments <= fx.audio_segments + 2,
        "audio re-downloaded: {c:?}"
    );
    support::oracles::assert_delivered_merged(&h.ffmpeg, &h.delivered_path(1));
    support::oracles::assert_no_stray_media_files(&h.work_dir_of(&job.id));
}

/// Value: protects=resume continuing from the breakpoint — the three fragments
/// completed before the pause are not re-fetched (a full restart would push
/// video requests past segments+2); fails_when=resume restarts the transfer
/// from zero, the pause drops the work dir, or delivery breaks; why_new=the
/// first-fragment pause cannot distinguish resume from restart under the
/// tolerance (observed when Task 5's mutation failed to go red); seam=none
#[tokio::test]
async fn resume_does_not_refetch_completed_fragments() {
    let h = support::harness::Harness::new(1);
    let job = h.enqueue(1);
    h.server.arm_gate_on_nth_fragment(1, 4); // hold the fourth fragment request
    assert!(h.server.wait_gate_engaged(Duration::from_secs(30)).await);
    let paused_at = h.server.counts(1);
    assert!(
        paused_at.video_fragments >= 4,
        "expected three completed fragments plus one held: {paused_at:?}"
    );

    h.manager.pause(&job.id).unwrap();
    h.wait_status(&job.id, JobStatus::Paused, Duration::from_secs(30))
        .await;
    h.server.release_gate();

    h.manager.resume(&job.id).unwrap();
    h.wait_status(&job.id, JobStatus::Done, Duration::from_secs(60))
        .await;

    let c = h.server.counts(1);
    let fx = support::fixture::ensure(&h.ffmpeg);
    assert!(
        c.video_fragments <= fx.video_segments + 2,
        "video re-downloaded: {c:?}"
    );
    assert!(
        c.audio_fragments <= fx.audio_segments + 2,
        "audio re-downloaded: {c:?}"
    );
    support::oracles::assert_delivered_merged(&h.ffmpeg, &h.delivered_path(1));
    support::oracles::assert_no_stray_media_files(&h.work_dir_of(&job.id));
}
