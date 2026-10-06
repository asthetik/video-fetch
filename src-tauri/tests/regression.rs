mod support;

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
