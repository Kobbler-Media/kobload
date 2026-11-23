use kob_usenet::{JobState, JobStatus};
use usenet_proto::{
    EngineActivity, JobActivity, JobPhase, ProviderActivity, StreamActivity, WaitReason,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct Holds {
    pub low_disk: bool,
    pub stream: bool,
}

#[must_use]
pub const fn job_phase(state: JobState) -> Option<JobPhase> {
    match state {
        JobState::Queued => Some(JobPhase::Queued),
        JobState::Downloading => Some(JobPhase::Downloading),
        JobState::Verifying => Some(JobPhase::Verifying),
        JobState::Repairing => Some(JobPhase::Repairing),
        JobState::Unpacking => Some(JobPhase::Unpacking),
        JobState::Completed | JobState::Failed => None,
    }
}

// The engine does not say which job a hold applies to, so a job counts as held only when a
// hold is in force and its own bytes have stopped.
const fn wait_reason(speed: Option<u64>, holds: Holds) -> Option<WaitReason> {
    match (speed, holds.low_disk, holds.stream) {
        (Some(0), true, _) => Some(WaitReason::LowDisk),
        (Some(0), false, true) => Some(WaitReason::Stream),
        _ => None,
    }
}

#[must_use]
pub fn job_activity(job: &JobStatus, speed: Option<u64>, holds: Holds) -> Option<JobActivity> {
    let phase = job_phase(job.state)?;
    let speed_bps = speed.filter(|_| phase == JobPhase::Downloading);
    let left = job.bytes_total.saturating_sub(job.bytes_done);
    let eta_seconds = speed_bps
        .filter(|&s| s > 0 && left > 0)
        .map(|s| left.div_ceil(s));
    Some(JobActivity {
        id: job.id.clone(),
        phase,
        downloaded_bytes: job.bytes_done,
        total_bytes: job.bytes_total,
        speed_bps,
        eta_seconds,
        waiting: wait_reason(speed_bps, holds),
    })
}

#[must_use]
pub fn engine_activity(
    low_disk: bool,
    providers: &[ProviderActivity],
    jobs: &[JobActivity],
    streams: &[StreamActivity],
) -> EngineActivity {
    if low_disk {
        return EngineActivity::PausedLowDisk;
    }
    if let Some(failing) = providers.iter().find(|p| p.failing) {
        return EngineActivity::ProviderFailing {
            host: failing.host.clone(),
        };
    }
    if jobs.iter().any(|j| j.waiting == Some(WaitReason::Stream)) {
        return EngineActivity::DownloadsWaitingForStream;
    }
    if !streams.is_empty() {
        return EngineActivity::Streaming;
    }
    if jobs.is_empty() {
        EngineActivity::Idle
    } else {
        EngineActivity::Downloading
    }
}
