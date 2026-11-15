//! Resource usage of running containers, worked out the way `docker stats`
//! does from the samples of the Docker stats API.
//!
//! The arithmetic is pure and lives here; `Client::follow_usage` feeds it.

use std::collections::HashMap;

use bollard::models::{
    ContainerBlkioStats, ContainerCpuStats, ContainerMemoryStats, ContainerNetworkStats,
    ContainerStatsResponse,
};

/// One container's usage at one moment, as a row of `docker stats`.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerUsage {
    pub id: String,
    /// The container name without Docker's leading `/`.
    pub name: String,
    /// CPU time used since the previous sample, in percent of one CPU as
    /// `docker stats` shows it: 250.0 means two and a half CPUs were busy,
    /// and the most it can be is `online_cpus * 100`. `None` for the first
    /// sample of a container, which has nothing to compare with.
    pub cpu_percent: Option<f64>,
    pub online_cpus: u32,
    /// Memory in use minus the inactive file cache, as `docker stats` shows.
    pub memory_used: u64,
    /// The container's memory limit; without one, the memory of the engine.
    pub memory_limit: u64,
    /// `memory_used` in percent of `memory_limit`; `None` without a limit.
    pub memory_percent: Option<f64>,
    /// Bytes received over all of the container's interfaces.
    pub net_rx: u64,
    /// Bytes sent over all of the container's interfaces.
    pub net_tx: u64,
    pub block_read: u64,
    pub block_write: u64,
    /// Processes and threads in the container.
    pub pids: u64,
}

/// The two cumulative counters a CPU percentage is worked out from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuCounters {
    /// Nanoseconds of CPU time the container used since it started.
    pub container: u64,
    /// Nanoseconds of CPU time the whole engine VM used since it booted,
    /// summed over all CPUs.
    pub system: u64,
}

impl CpuCounters {
    /// The counters of one sample. `None` when the engine left the system
    /// counter out or at zero, which it does for the empty `precpu_stats` of
    /// a one-shot sample and for a container that stopped.
    pub fn of(stats: &ContainerCpuStats) -> Option<Self> {
        let system = stats.system_cpu_usage.filter(|&system| system > 0)?;
        let container = stats.cpu_usage.as_ref()?.total_usage?;
        Some(Self { container, system })
    }
}

/// `docker stats`' CPU percent between two samples: the container's share of
/// the CPU time that passed, scaled to the number of CPUs. Zero when either
/// counter did not move forward, as in `docker stats`.
pub fn cpu_percent(previous: CpuCounters, current: CpuCounters, online_cpus: u32) -> f64 {
    let container = current.container as f64 - previous.container as f64;
    let system = current.system as f64 - previous.system as f64;
    if container > 0.0 && system > 0.0 {
        container / system * f64::from(online_cpus) * 100.0
    } else {
        0.0
    }
}

/// CPUs the engine VM has online. Older kernels only reported a per-CPU
/// list, whose length `docker stats` falls back to.
pub fn online_cpus(stats: &ContainerCpuStats) -> u32 {
    stats
        .online_cpus
        .filter(|&cpus| cpus > 0)
        .unwrap_or_else(|| {
            let per_cpu = stats
                .cpu_usage
                .as_ref()
                .and_then(|u| u.percpu_usage.as_ref());
            per_cpu.map_or(0, |list| u32::try_from(list.len()).unwrap_or(u32::MAX))
        })
}

/// Memory in use without the page cache the kernel can drop at once:
/// `total_inactive_file` on cgroup v1, `inactive_file` on cgroup v2.
pub fn memory_used(memory: &ContainerMemoryStats) -> u64 {
    let usage = memory.usage.unwrap_or(0);
    let stats = memory.stats.as_ref();
    let inactive = stats
        .and_then(|stats| stats.get("total_inactive_file"))
        .or_else(|| stats.and_then(|stats| stats.get("inactive_file")));
    match inactive {
        Some(&inactive) if inactive < usage => usage - inactive,
        _ => usage,
    }
}

/// Bytes received and sent, summed over the interfaces.
pub fn network_bytes(networks: &HashMap<String, ContainerNetworkStats>) -> (u64, u64) {
    networks.values().fold((0, 0), |(rx, tx), net| {
        (
            rx.saturating_add(net.rx_bytes.unwrap_or(0)),
            tx.saturating_add(net.tx_bytes.unwrap_or(0)),
        )
    })
}

/// Bytes read and written. On cgroup v2 only `io_service_bytes_recursive`
/// is filled, with lower-case operation names; v1 capitalises them.
pub fn block_io(blkio: &ContainerBlkioStats) -> (u64, u64) {
    let entries = blkio.io_service_bytes_recursive.iter().flatten();
    entries.fold((0, 0), |(read, write), entry| {
        let value = entry.value.unwrap_or(0);
        match entry.op.as_deref().and_then(|op| op.chars().next()) {
            Some('r' | 'R') => (read.saturating_add(value), write),
            Some('w' | 'W') => (read, write.saturating_add(value)),
            _ => (read, write),
        }
    })
}

/// True for the sample the engine sends for a container that is not running:
/// no read time (Go's zero time) and no memory figures.
pub fn is_stopped(sample: &ContainerStatsResponse) -> bool {
    let never_read = sample
        .read
        .as_deref()
        .is_none_or(|read| read.is_empty() || read.starts_with("0001-01-01"));
    let no_memory = sample
        .memory_stats
        .as_ref()
        .is_none_or(|memory| memory.usage.is_none());
    never_read || no_memory
}

/// Turns rounds of stats samples into usage rows, remembering each
/// container's CPU counters from one round to the next.
#[derive(Debug, Default)]
pub struct UsageTracker {
    previous: HashMap<String, CpuCounters>,
}

impl UsageTracker {
    /// The rows for one round of samples, sorted by name. A container missing
    /// from the round, or whose sample says it stopped, has no row and is
    /// forgotten, so if it starts again its CPU is measured afresh.
    pub fn update(
        &mut self,
        samples: impl IntoIterator<Item = ContainerStatsResponse>,
    ) -> Vec<ContainerUsage> {
        let mut current = HashMap::new();
        let mut rows = Vec::new();
        for sample in samples {
            let Some(id) = sample.id.clone().filter(|id| !id.is_empty()) else {
                continue;
            };
            if is_stopped(&sample) {
                continue;
            }
            let previous = self.previous.get(&id).copied();
            let (row, counters) = usage_of(id.clone(), &sample, previous);
            if let Some(counters) = counters {
                current.insert(id, counters);
            }
            rows.push(row);
        }
        self.previous = current;
        rows.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        rows
    }
}

/// One row from one sample. `previous` are the counters of this container's
/// last sample; without them the sample's own `precpu_stats` are used when
/// they hold data (a streamed sample), and otherwise the CPU is unknown.
/// Also returns the sample's counters, for the next round.
fn usage_of(
    id: String,
    sample: &ContainerStatsResponse,
    previous: Option<CpuCounters>,
) -> (ContainerUsage, Option<CpuCounters>) {
    let cpu = sample.cpu_stats.clone().unwrap_or_default();
    let counters = CpuCounters::of(&cpu);
    let online = online_cpus(&cpu);
    let previous = previous.or_else(|| sample.precpu_stats.as_ref().and_then(CpuCounters::of));
    let cpu_percent = counters
        .zip(previous)
        .map(|(current, previous)| cpu_percent(previous, current, online));

    let memory = sample.memory_stats.clone().unwrap_or_default();
    let memory_used = memory_used(&memory);
    let memory_limit = memory.limit.unwrap_or(0);
    let memory_percent =
        (memory_limit > 0).then(|| memory_used as f64 / memory_limit as f64 * 100.0);

    let (net_rx, net_tx) = sample.networks.as_ref().map_or((0, 0), network_bytes);
    let (block_read, block_write) = sample.blkio_stats.as_ref().map_or((0, 0), block_io);
    let pids = sample
        .pids_stats
        .as_ref()
        .and_then(|pids| pids.current)
        .unwrap_or(0);
    let name = sample
        .name
        .as_deref()
        .map(|name| name.trim_start_matches('/').to_string())
        .unwrap_or_default();

    let row = ContainerUsage {
        id,
        name,
        cpu_percent,
        online_cpus: online,
        memory_used,
        memory_limit,
        memory_percent,
        net_rx,
        net_tx,
        block_read,
        block_write,
        pids,
    };
    (row, counters)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-shot sample on a cgroup v2 engine, as `GET
    /// /containers/{id}/stats?stream=false&one-shot=true` returns it:
    /// `precpu_stats` is empty.
    const ONE_SHOT_V2: &str = r#"{
        "name": "/web",
        "id": "4f1c0e9e6d0b",
        "os_type": "linux",
        "read": "2026-11-15T09:30:01.512345678Z",
        "preread": "0001-01-01T00:00:00Z",
        "pids_stats": { "current": 7, "limit": 18446744073709551615 },
        "blkio_stats": {
            "io_service_bytes_recursive": [
                { "major": 8, "minor": 16, "op": "read", "value": 4096000 },
                { "major": 8, "minor": 16, "op": "write", "value": 12288 },
                { "major": 8, "minor": 32, "op": "read", "value": 4000 }
            ],
            "io_serviced_recursive": null,
            "io_queue_recursive": null,
            "io_service_time_recursive": null,
            "io_wait_time_recursive": null,
            "io_merged_recursive": null,
            "io_time_recursive": null,
            "sectors_recursive": null
        },
        "num_procs": 0,
        "storage_stats": {},
        "cpu_stats": {
            "cpu_usage": {
                "total_usage": 2000000000,
                "usage_in_kernelmode": 500000000,
                "usage_in_usermode": 1500000000
            },
            "system_cpu_usage": 100000000000,
            "online_cpus": 4,
            "throttling_data": { "periods": 0, "throttled_periods": 0, "throttled_time": 0 }
        },
        "precpu_stats": {
            "cpu_usage": { "total_usage": 0, "usage_in_kernelmode": 0, "usage_in_usermode": 0 },
            "throttling_data": { "periods": 0, "throttled_periods": 0, "throttled_time": 0 }
        },
        "memory_stats": {
            "usage": 52428800,
            "stats": {
                "active_anon": 8192000,
                "active_file": 4096000,
                "anon": 31457280,
                "file": 20971520,
                "inactive_anon": 23265280,
                "inactive_file": 10485760,
                "kernel_stack": 114688,
                "pgfault": 15000,
                "shmem": 0,
                "slab": 900000
            },
            "limit": 8317095936
        },
        "networks": {
            "eth0": {
                "rx_bytes": 1500, "rx_packets": 20, "rx_errors": 0, "rx_dropped": 0,
                "tx_bytes": 700, "tx_packets": 9, "tx_errors": 0, "tx_dropped": 0
            },
            "eth1": {
                "rx_bytes": 500, "rx_packets": 4, "rx_errors": 0, "rx_dropped": 0,
                "tx_bytes": 300, "tx_packets": 3, "tx_errors": 0, "tx_dropped": 0
            }
        }
    }"#;

    /// The same container one second later, streamed: `precpu_stats` holds
    /// the counters of the sample before.
    const STREAMED_V2: &str = r#"{
        "name": "/web",
        "id": "4f1c0e9e6d0b",
        "read": "2026-11-15T09:30:02.513000000Z",
        "preread": "2026-11-15T09:30:01.512345678Z",
        "pids_stats": { "current": 8 },
        "blkio_stats": { "io_service_bytes_recursive": [] },
        "cpu_stats": {
            "cpu_usage": { "total_usage": 2500000000 },
            "system_cpu_usage": 104000000000,
            "online_cpus": 4
        },
        "precpu_stats": {
            "cpu_usage": { "total_usage": 2000000000 },
            "system_cpu_usage": 100000000000,
            "online_cpus": 4
        },
        "memory_stats": {
            "usage": 52428800,
            "stats": { "inactive_file": 10485760 },
            "limit": 8317095936
        }
    }"#;

    /// A cgroup v1 sample, with capitalised block I/O operations, the cache
    /// under `total_inactive_file` and only a per-CPU list.
    const STREAMED_V1: &str = r#"{
        "name": "/db",
        "id": "9a8b7c6d5e4f",
        "read": "2026-11-15T09:30:02Z",
        "pids_stats": { "current": 31 },
        "blkio_stats": {
            "io_service_bytes_recursive": [
                { "major": 8, "minor": 0, "op": "Read", "value": 1000 },
                { "major": 8, "minor": 0, "op": "Write", "value": 2000 },
                { "major": 8, "minor": 0, "op": "Sync", "value": 3000 },
                { "major": 8, "minor": 0, "op": "Async", "value": 0 },
                { "major": 8, "minor": 0, "op": "Total", "value": 3000 }
            ]
        },
        "cpu_stats": {
            "cpu_usage": { "total_usage": 300, "percpu_usage": [100, 100, 50, 50] },
            "system_cpu_usage": 2000
        },
        "precpu_stats": {
            "cpu_usage": { "total_usage": 100, "percpu_usage": [50, 50, 0, 0] },
            "system_cpu_usage": 1000
        },
        "memory_stats": {
            "usage": 1000,
            "max_usage": 1200,
            "stats": { "cache": 300, "total_inactive_file": 200, "inactive_file": 999 },
            "limit": 4000
        }
    }"#;

    /// What the engine sends for a container that just stopped.
    const STOPPED: &str = r#"{
        "name": "/web",
        "id": "4f1c0e9e6d0b",
        "read": "0001-01-01T00:00:00Z",
        "preread": "0001-01-01T00:00:00Z",
        "pids_stats": {},
        "blkio_stats": {
            "io_service_bytes_recursive": null,
            "io_serviced_recursive": null
        },
        "num_procs": 0,
        "storage_stats": {},
        "cpu_stats": { "cpu_usage": { "total_usage": 0 }, "throttling_data": {} },
        "precpu_stats": { "cpu_usage": { "total_usage": 0 }, "throttling_data": {} },
        "memory_stats": {}
    }"#;

    fn sample(json: &str) -> ContainerStatsResponse {
        serde_json::from_str(json).expect("a valid stats fixture")
    }

    fn close(actual: f64, expected: f64) -> bool {
        (actual - expected).abs() < 1e-9
    }

    #[test]
    fn cpu_percent_is_the_share_of_system_time_times_the_cpus() {
        let previous = CpuCounters {
            container: 2_000_000_000,
            system: 100_000_000_000,
        };
        let current = CpuCounters {
            container: 2_500_000_000,
            system: 104_000_000_000,
        };
        // Half a second of CPU in a second of four CPUs: half of one CPU.
        assert!(close(cpu_percent(previous, current, 4), 50.0));
    }

    #[test]
    fn cpu_percent_is_zero_when_a_counter_stands_still_or_goes_back() {
        let at = |container, system| CpuCounters { container, system };
        assert_eq!(cpu_percent(at(10, 100), at(10, 200), 4), 0.0);
        assert_eq!(cpu_percent(at(10, 100), at(20, 100), 4), 0.0);
        // A restarted container starts its counter again from zero.
        assert_eq!(cpu_percent(at(500, 100), at(10, 200), 4), 0.0);
    }

    #[test]
    fn cpu_percent_can_exceed_one_cpu() {
        let previous = CpuCounters {
            container: 0,
            system: 1000,
        };
        let current = CpuCounters {
            container: 750,
            system: 2000,
        };
        assert!(close(cpu_percent(previous, current, 4), 300.0));
    }

    #[test]
    fn a_streamed_sample_carries_its_own_previous_counters() {
        let mut tracker = UsageTracker::default();
        let rows = tracker.update([sample(STREAMED_V2)]);
        assert_eq!(rows.len(), 1);
        assert!(close(rows[0].cpu_percent.unwrap(), 50.0));
        assert_eq!(rows[0].pids, 8);
    }

    #[test]
    fn cgroup_v2_sample_without_a_previous_one() {
        let mut tracker = UsageTracker::default();
        let rows = tracker.update([sample(ONE_SHOT_V2)]);
        let row = &rows[0];
        assert_eq!(row.id, "4f1c0e9e6d0b");
        assert_eq!(row.name, "web");
        // Nothing to compare the first one-shot sample with.
        assert_eq!(row.cpu_percent, None);
        assert_eq!(row.online_cpus, 4);
        // 50 MiB used, 10 MiB of it inactive file cache.
        assert_eq!(row.memory_used, 40 * 1024 * 1024);
        assert_eq!(row.memory_limit, 8_317_095_936);
        let percent = row.memory_percent.unwrap();
        assert!(close(percent, 41_943_040.0 / 8_317_095_936.0 * 100.0));
        assert_eq!((row.net_rx, row.net_tx), (2000, 1000));
        assert_eq!((row.block_read, row.block_write), (4_100_000, 12288));
        assert_eq!(row.pids, 7);
    }

    #[test]
    fn the_next_round_measures_cpu_against_the_last_one() {
        let mut tracker = UsageTracker::default();
        tracker.update([sample(ONE_SHOT_V2)]);
        let mut next = sample(ONE_SHOT_V2);
        let cpu = next.cpu_stats.as_mut().unwrap();
        cpu.cpu_usage.as_mut().unwrap().total_usage = Some(3_000_000_000);
        cpu.system_cpu_usage = Some(108_000_000_000);
        let rows = tracker.update([next]);
        // One second of CPU in two seconds of four CPUs.
        assert!(close(rows[0].cpu_percent.unwrap(), 50.0));
    }

    #[test]
    fn cgroup_v1_sample() {
        let mut tracker = UsageTracker::default();
        let row = tracker.update([sample(STREAMED_V1)]).remove(0);
        // v1's own counter wins over a stray v2 one.
        assert_eq!(row.memory_used, 800);
        assert!(close(row.memory_percent.unwrap(), 20.0));
        // No `online_cpus`: the per-CPU list has four entries.
        assert_eq!(row.online_cpus, 4);
        assert!(close(row.cpu_percent.unwrap(), 80.0));
        assert_eq!((row.block_read, row.block_write), (1000, 2000));
        assert_eq!((row.net_rx, row.net_tx), (0, 0));
    }

    #[test]
    fn a_stopped_container_has_no_row_and_is_forgotten() {
        assert!(is_stopped(&sample(STOPPED)));
        assert!(!is_stopped(&sample(ONE_SHOT_V2)));

        let mut tracker = UsageTracker::default();
        tracker.update([sample(ONE_SHOT_V2)]);
        assert!(tracker.update([sample(STOPPED)]).is_empty());
        // Started again: measured afresh, not against the counters from
        // before it stopped.
        let rows = tracker.update([sample(ONE_SHOT_V2)]);
        assert_eq!(rows[0].cpu_percent, None);
    }

    #[test]
    fn containers_missing_from_a_round_are_dropped() {
        let mut tracker = UsageTracker::default();
        let rows = tracker.update([sample(ONE_SHOT_V2), sample(STREAMED_V1)]);
        let names: Vec<_> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["db", "web"]);

        let rows = tracker.update([sample(STREAMED_V1)]);
        assert_eq!(rows.len(), 1);
        assert_eq!(tracker.previous.len(), 1);
        assert!(tracker.update([]).is_empty());
        assert!(tracker.previous.is_empty());
    }

    #[test]
    fn memory_is_usage_when_the_cache_figure_is_missing_or_too_big() {
        let memory = |usage, stats: &[(&str, u64)]| ContainerMemoryStats {
            usage: Some(usage),
            stats: Some(stats.iter().map(|(k, v)| (k.to_string(), *v)).collect()),
            ..Default::default()
        };
        assert_eq!(memory_used(&memory(1000, &[])), 1000);
        assert_eq!(memory_used(&memory(1000, &[("inactive_file", 1000)])), 1000);
        assert_eq!(memory_used(&memory(1000, &[("inactive_file", 400)])), 600);
        assert_eq!(memory_used(&ContainerMemoryStats::default()), 0);
    }

    #[test]
    fn counters_need_a_system_figure() {
        let empty = sample(ONE_SHOT_V2).precpu_stats.unwrap();
        assert_eq!(CpuCounters::of(&empty), None);
        assert_eq!(CpuCounters::of(&ContainerCpuStats::default()), None);
    }
}
