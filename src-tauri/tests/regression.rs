mod support;

use std::time::Duration;

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
