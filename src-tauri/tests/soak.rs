mod support;

use std::collections::HashSet;
use std::time::Duration;

use support::{harness::Harness, oracles, rng::XorShift64};
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

        let op = match rng.below(100) {
            0..=29 if jobs.len() < max_jobs => Op::Enqueue,
            30..=49 if !pending_running.is_empty() => Op::Pause,
            50..=64 if !paused.is_empty() => Op::Resume,
            65..=76 if !jobs.is_empty() => Op::Cancel,
            77..=84 if !failed.is_empty() => Op::Retry,
            85..=89 if !done.is_empty() || !failed.is_empty() => Op::Delete,
            90..=93 => Op::PauseAll,
            94..=96 => Op::ResumeAll,
            97..=98 => Op::CrashClean,
            _ => Op::CrashRestart,
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

    // --- drain ---
    let _ = h.manager.resume_all();
    let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let jobs = list_retry(&h).await;
        let busy = jobs
            .iter()
            .any(|j| matches!(j.status, JobStatus::Pending | JobStatus::Running));
        if !busy {
            break;
        }
        assert!(
            tokio::time::Instant::now() < drain_deadline,
            "drain timed out: {jobs:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // --- invariants ---
    let jobs = list_retry(&h).await;
    for j in &jobs {
        assert_ne!(j.status, JobStatus::Running, "zombie Running row: {}", j.id);
        if j.status == JobStatus::Failed {
            assert!(
                cancelled.contains(&j.id) || crash_era.contains(&j.id),
                "failure not caused by a cancel or crash: {} {:?}",
                j.id,
                j.error
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
    assert_eq!(snapshot, after, "requests kept flowing after drain");

    // Reap any orphans the crash cycles left (crashes may orphan processes; the drain must still converge)
    oracles::kill_processes_containing(&h.marker());
    oracles::wait_no_stray_processes(&h.marker(), Duration::from_secs(10)).await;

    eprintln!("soak: done seed={seed}; ops={}", ledger.len());
    eprintln!("soak: ledger: {ledger:?}");
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
