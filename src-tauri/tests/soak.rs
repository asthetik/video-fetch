mod support;

use std::collections::HashSet;
use std::time::Duration;

use support::{harness::Harness, oracles, rng::XorShift64, server::RequestCounts};
use video_fetch_lib::testing::{DownloadJob, JobStatus};

#[derive(Clone, Copy, Debug)]
enum Op {
    Enqueue,
    Pause,
    Resume,
    Cancel,
    Retry,
    Delete,
    PauseAll,
    ResumeAll,
    CrashClean,
    CrashRestart,
}

/// Bound on guard-driven re-rolls per driver iteration; the fallback after the
/// limit is PauseAll (always applicable — a no-op on an empty queue).
const REROLL_LIMIT: usize = 8;

#[tokio::test]
#[ignore = "soak: run locally, never in CI"]
async fn soak_churns_downloads_and_asserts_invariants() {
    let minutes: u64 = std::env::var("SOAK_MINUTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let seed: u64 = std::env::var("SOAK_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        });
    let max_jobs: usize = std::env::var("SOAK_JOBS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6);

    let mut rng = XorShift64::new(seed);
    let mut ledger: Vec<(Op, String)> = Vec::new();
    // Failures allowed through the drain: ids the driver cancelled, plus ids
    // active when a crash/restart op ran (their runners may legitimately error
    // out; a real crash would kill the process before it wrote anything).
    // Both exemptions expire when the driver re-drives a job (Retry, Resume,
    // resume_all): a failure after a re-drive is a real finding.
    let mut cancelled: HashSet<String> = HashSet::new();
    let mut crash_era: HashSet<String> = HashSet::new();
    let mut next_n = 1usize;

    let mut h = Harness::new(2);
    eprintln!("soak: seed={seed} minutes={minutes} jobs<={max_jobs}");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(minutes * 60);
    while tokio::time::Instant::now() < deadline {
        let jobs = list_retry(&h).await;
        let pending_running: Vec<&DownloadJob> = jobs
            .iter()
            .filter(|j| matches!(j.status, JobStatus::Pending | JobStatus::Running))
            .collect();
        let paused: Vec<&DownloadJob> = jobs
            .iter()
            .filter(|j| j.status == JobStatus::Paused)
            .collect();
        let failed: Vec<&DownloadJob> = jobs
            .iter()
            .filter(|j| j.status == JobStatus::Failed)
            .collect();
        let done: Vec<&DownloadJob> = jobs
            .iter()
            .filter(|j| j.status == JobStatus::Done)
            .collect();

        // A draw whose arm is not applicable re-rolls (bounded) instead of
        // falling through to the catch-all arm: plain match fall-through
        // collapsed every skipped weight into CrashRestart (observed 54% vs
        // 1% nominal). PauseAll is the fallback after the limit — always
        // applicable.
        let op = 'draw: {
            for _ in 0..REROLL_LIMIT {
                let candidate = match rng.below(100) {
                    0..=29 => Op::Enqueue,
                    30..=49 => Op::Pause,
                    50..=64 => Op::Resume,
                    65..=76 => Op::Cancel,
                    77..=84 => Op::Retry,
                    85..=89 => Op::Delete,
                    90..=93 => Op::PauseAll,
                    94..=96 => Op::ResumeAll,
                    97..=98 => Op::CrashClean,
                    _ => Op::CrashRestart,
                };
                let applicable = match candidate {
                    Op::Enqueue => jobs.len() < max_jobs,
                    Op::Pause => !pending_running.is_empty(),
                    Op::Resume => !paused.is_empty(),
                    Op::Cancel => !jobs.is_empty(),
                    Op::Retry => !failed.is_empty(),
                    Op::Delete => !done.is_empty() || !failed.is_empty(),
                    Op::PauseAll | Op::ResumeAll | Op::CrashClean | Op::CrashRestart => true,
                };
                if applicable {
                    break 'draw candidate;
                }
            }
            Op::PauseAll
        };
        // Concurrent bursts: ops are not awaited one by one — overlapping windows are the point.
        match op {
            Op::Enqueue => {
                let n = next_n;
                next_n += 1;
                let job = h.job_for(n); // built here: job_for borrows &h; the task only needs the value
                ledger.push((op, job.video_id.clone()));
                let m = h.manager.clone();
                tokio::spawn(async move {
                    let _ = m.enqueue(job, false);
                });
            }
            Op::Pause => {
                let id = pending_running[rng.below(pending_running.len() as u64) as usize]
                    .id
                    .clone();
                ledger.push((op, id.clone()));
                let m = h.manager.clone();
                tokio::spawn(async move {
                    let _ = m.pause(&id);
                });
            }
            Op::Resume => {
                let id = paused[rng.below(paused.len() as u64) as usize].id.clone();
                ledger.push((op, id.clone()));
                cancelled.remove(&id);
                crash_era.remove(&id);
                let m = h.manager.clone();
                tokio::spawn(async move {
                    let _ = m.resume(&id);
                });
            }
            Op::Cancel => {
                let id = jobs[rng.below(jobs.len() as u64) as usize].id.clone();
                ledger.push((op, id.clone()));
                cancelled.insert(id.clone());
                let m = h.manager.clone();
                tokio::spawn(async move {
                    let _ = m.cancel(&id);
                });
            }
            Op::Retry => {
                let id = failed[rng.below(failed.len() as u64) as usize].id.clone();
                ledger.push((op, id.clone()));
                cancelled.remove(&id);
                // Re-driving revokes the crash-era exemption: a failure after
                // this retry is a real finding, not a crash-era artifact.
                crash_era.remove(&id);
                let m = h.manager.clone();
                tokio::spawn(async move {
                    let _ = m.retry(&id);
                });
            }
            Op::Delete => {
                // Source from whichever list is non-empty: the rng guard allows
                // `done` empty while `failed` is not.
                let id = if !done.is_empty() {
                    done[rng.below(done.len() as u64) as usize].id.clone()
                } else {
                    failed[rng.below(failed.len() as u64) as usize].id.clone()
                };
                ledger.push((op, id.clone()));
                let m = h.manager.clone();
                tokio::spawn(async move {
                    let _ = m.delete(&id, true);
                });
            }
            Op::PauseAll => {
                ledger.push((op, String::new()));
                let m = h.manager.clone();
                tokio::spawn(async move {
                    let _ = m.pause_all();
                });
            }
            Op::ResumeAll => {
                ledger.push((op, String::new()));
                // Re-driving revokes the crash-era exemption: a failure after
                // this resume is a real finding, not a crash-era artifact.
                for j in &paused {
                    crash_era.remove(&j.id);
                }
                let m = h.manager.clone();
                tokio::spawn(async move {
                    let _ = m.resume_all();
                });
            }
            Op::CrashClean => {
                ledger.push((op, String::new()));
                for j in &pending_running {
                    crash_era.insert(j.id.clone());
                }
                h.clean_restart();
            }
            Op::CrashRestart => {
                ledger.push((op, String::new()));
                for j in &pending_running {
                    crash_era.insert(j.id.clone());
                }
                h.crash_restart();
                // Orphans linger the way a real crash leaves them; reap after a
                // grace period so the drain can converge.
                tokio::time::sleep(Duration::from_millis(400)).await;
                oracles::kill_processes_containing(&h.marker());
            }
        }
        tokio::time::sleep(Duration::from_millis(50 + rng.below(150))).await;
    }

    // Print the ledger before the drain too: an invariant failure must surface
    // the op sequence that produced it even when the drain or an assertion
    // panics later.
    eprintln!("soak: main loop done seed={seed}; ops={}", ledger.len());
    eprintln!("soak: ledger: {ledger:?}");

    // --- drain ---
    // Same rule as the in-loop ResumeAll: scrub every currently-Paused id
    // before the drain re-drives them, so a failure after this resume is a
    // real finding instead of a masked crash-era artifact.
    let before_drain = list_retry(&h).await;
    for j in before_drain
        .iter()
        .filter(|j| j.status == JobStatus::Paused)
    {
        crash_era.remove(&j.id);
    }
    let _ = h.manager.resume_all();
    let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    // Log the busy set on every change: a drain that converges late (or a row
    // that flips back to Pending/Running) must be reconstructable from the log.
    let mut drain_ticks = 0usize;
    let mut last_busy = String::new();
    loop {
        let jobs = list_retry(&h).await;
        let busy: Vec<String> = jobs
            .iter()
            .filter(|j| matches!(j.status, JobStatus::Pending | JobStatus::Running))
            .map(|j| format!("{}:{:?}", j.id, j.status))
            .collect();
        drain_ticks += 1;
        let busy_sig = busy.join(",");
        if busy_sig != last_busy {
            eprintln!("soak: drain tick {drain_ticks}: busy=[{busy_sig}]");
            last_busy = busy_sig.clone();
        }
        if busy.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < drain_deadline,
            "drain timed out: {jobs:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // --- quiesce the crash era ---
    // A real crash kills these processes with the app; the in-process
    // approximation only reaps around crash ops, and a runner queued on a
    // dropped world's semaphore can wake after every one of those scans and
    // start an untracked download behind a terminal row. With the rows quiet
    // any process still carrying the marker belongs to such a leaked runner,
    // so reap until none survive (a leaked runner woken later reads a quiet
    // row and stands down). Without this the silence window below would
    // measure the simulation's zombies instead of the drained system.
    for _ in 0..3 {
        oracles::kill_processes_containing(&h.marker());
        tokio::time::sleep(Duration::from_millis(300)).await;
        if oracles::live_processes_containing(&h.marker()).is_empty() {
            break;
        }
    }
    oracles::wait_no_stray_processes(&h.marker(), Duration::from_secs(10)).await;
    // Settle: a reaped runner lands its terminal write before the snapshot.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // --- invariants ---
    let jobs = list_retry(&h).await;
    // Evidence shipped with any failure below: the row set and any live
    // processes at drain convergence. argv carries the work dir (hence the job
    // id), so a stray here names the row it belongs to.
    eprintln!("soak: rows at drain: {}", row_summary(&jobs));
    let strays = oracles::live_processes_containing(&h.marker());
    if !strays.is_empty() {
        eprintln!("soak: live processes at drain: {strays:?}");
    }
    for j in &jobs {
        assert_ne!(j.status, JobStatus::Running, "zombie Running row: {}", j.id);
        if j.status == JobStatus::Failed {
            assert!(
                cancelled.contains(&j.id) || crash_era.contains(&j.id),
                "failure not caused by a cancel or crash: {j:?}; in cancelled={} in crash_era={}; \
                 work dir: {:?}; live processes: {:?}",
                cancelled.contains(&j.id),
                crash_era.contains(&j.id),
                work_dir_listing(&h, &j.id),
                oracles::live_processes_containing(&h.marker())
            );
        }
        if j.status == JobStatus::Paused {
            assert!(
                h.work_dir_of(&j.id).is_dir(),
                "paused job lost its work dir: {}",
                j.id
            );
        }
    }
    // After a silence window, request counts must stop growing (auxiliary oracle)
    let snapshot: Vec<_> = (1..next_n).map(|n| h.server.counts(n)).collect();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let after: Vec<_> = (1..next_n).map(|n| h.server.counts(n)).collect();
    // Report only the videos whose counters moved, plus any live processes:
    // the full 10k-entry lists are unreadable and hide the signal.
    let changed: Vec<(usize, RequestCounts, RequestCounts)> = snapshot
        .iter()
        .zip(&after)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, (a, b))| (i + 1, *a, *b))
        .collect();
    assert!(
        changed.is_empty(),
        "requests kept flowing after drain on videos {changed:?}; live processes: {:?}",
        oracles::live_processes_containing(&h.marker())
    );

    // Reap any orphans the crash cycles left (crashes may orphan processes; the drain must still converge)
    oracles::kill_processes_containing(&h.marker());
    oracles::wait_no_stray_processes(&h.marker(), Duration::from_secs(10)).await;

    eprintln!("soak: done seed={seed}; ops={}", ledger.len());
    eprintln!("soak: ledger: {ledger:?}");
}

/// One-line id/status pairs for the failure evidence.
fn row_summary(jobs: &[DownloadJob]) -> String {
    jobs.iter()
        .map(|j| format!("{}={:?}", j.id, j.status))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Entry names of a job's work dir; `["gone"]` when the dir is absent.
fn work_dir_listing(h: &Harness, id: &str) -> Vec<String> {
    match std::fs::read_dir(h.work_dir_of(id)) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => vec!["gone".into()],
    }
}

/// Status reads may hit SQLITE_BUSY while a crash-era runner (leaked by design)
/// still writes the shared db — retry briefly instead of panicking.
async fn list_retry(h: &Harness) -> Vec<DownloadJob> {
    for _ in 0..40 {
        if let Ok(jobs) = h.manager.list() {
            return jobs;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("list() kept failing (sqlite contention) after 2s");
}
