use serde::{Deserialize, Serialize};

/// Owned, JSON-friendly mirror of `whyslow_common::Event`. The BPF side stays
/// `no_std`/allocation-free; this is where we pay one allocation per event to
/// get a `String` comm and serde support for the on-disk trace format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    pub timestamp_ns: u64,
    pub duration_ns: u64,
    pub key: u64,
    pub pid: u32,
    pub tid: u32,
    pub event_type: u8,
    pub comm: String,
}

impl From<whyslow_common::Event> for TraceEvent {
    fn from(ev: whyslow_common::Event) -> Self {
        let comm_end = ev.comm.iter().position(|&b| b == 0).unwrap_or(ev.comm.len());
        Self {
            timestamp_ns: ev.timestamp_ns,
            duration_ns: ev.duration_ns,
            key: ev.key,
            pid: ev.pid,
            tid: ev.tid,
            event_type: ev.event_type,
            comm: String::from_utf8_lossy(&ev.comm[..comm_end]).into_owned(),
        }
    }
}

/// On-disk trace format written by `run`/`attach` and read back by `explain`.
///
/// BPF timestamps are `CLOCK_MONOTONIC`-ish boot time (`bpf_ktime_get_ns`),
/// which is meaningless outside the machine/boot that produced it. We store a
/// same-instant `(realtime, monotonic)` anchor pair captured right before
/// tracing starts so any later reader can recover wall-clock time.
#[derive(Debug, Serialize, Deserialize)]
pub struct TraceFile {
    pub realtime_anchor_ns: u64,
    pub monotonic_anchor_ns: u64,
    pub events: Vec<TraceEvent>,
}

impl TraceFile {
    pub fn capture_anchor() -> (u64, u64) {
        let realtime = clock_ns(libc::CLOCK_REALTIME);
        let monotonic = clock_ns(libc::CLOCK_MONOTONIC);
        (realtime, monotonic)
    }

    pub fn wall_ns_for(&self, event_ts_ns: u64) -> u64 {
        let delta = event_ts_ns as i128 - self.monotonic_anchor_ns as i128;
        (self.realtime_anchor_ns as i128 + delta).max(0) as u64
    }

    pub fn write_to(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let file = std::fs::File::create(path)?;
        serde_json::to_writer_pretty(file, self)?;
        Ok(())
    }

    pub fn read_from(path: &std::path::Path) -> anyhow::Result<Self> {
        let file = std::fs::File::open(path)?;
        Ok(serde_json::from_reader(file)?)
    }
}

fn clock_ns(clock: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(clock, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}
