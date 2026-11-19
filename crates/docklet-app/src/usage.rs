//! What the engine costs, as a strip of meters: memory, CPU and GPU of the
//! WSL VM, the engine's disk, and what Docker keeps on it.
//!
//! Every figure can be missing for a reason of its own: the VM process
//! cannot be told apart, the GPU publishes no counters, a rate has only one
//! sample so far, the disk usage was never asked for. Each of those is a
//! state a meter shows, never an error.

use docklet_engine::{ContainerUsage, DiskUsage};
use docklet_wsl::usage::{HostUsage, VmStatus, VmUsage};

use crate::format::{UNKNOWN, format_bytes, format_percent};
use crate::streams::HostReading;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterKind {
    Memory,
    Cpu,
    Gpu,
    /// The distro's virtual disk on the Windows drive.
    Disk,
    /// What images, containers, volumes and the build cache take up inside
    /// that disk.
    DockerData,
}

/// Why a meter shows what it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterState {
    /// `value` is a figure.
    Known,
    /// The figure is on its way: the first sample has not arrived, a rate
    /// needs a second one, or the disk usage is being worked out.
    Measuring,
    /// There is no figure and waiting will not bring one; `detail` says why.
    Unknown,
    /// There is no figure because nobody asked for it: Docker's disk usage
    /// is only worked out on demand.
    NotMeasured,
    /// Nothing to measure: the WSL VM is not running.
    Off,
}

/// One meter of the strip.
#[derive(Debug, Clone, PartialEq)]
pub struct Meter {
    pub kind: MeterKind,
    pub label: &'static str,
    /// The figure, or `UNKNOWN`, or `Off`.
    pub value: String,
    /// A sentence for a tooltip: what the figure is, or why there is none.
    pub detail: String,
    /// How full to draw a bar, from 0 to 1, where the figure has a natural
    /// whole. `None` draws no bar.
    pub fraction: Option<f64>,
    pub state: MeterState,
}

impl Meter {
    fn new(kind: MeterKind, state: MeterState, value: &str, detail: impl Into<String>) -> Self {
        let label = match kind {
            MeterKind::Memory => "Memory",
            MeterKind::Cpu => "CPU",
            MeterKind::Gpu => "GPU",
            MeterKind::Disk => "Disk",
            MeterKind::DockerData => "Docker data",
        };
        Self {
            kind,
            label,
            value: value.to_string(),
            detail: detail.into(),
            fraction: None,
            state,
        }
    }

    fn known(kind: MeterKind, value: String, detail: impl Into<String>) -> Self {
        Self::new(kind, MeterState::Known, &value, detail)
    }

    fn filled(mut self, fraction: f64) -> Self {
        self.fraction = Some(fraction.clamp(0.0, 1.0));
        self
    }
}

/// Docker's disk usage, which is only worked out when asked for.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum DiskReading {
    /// Nobody asked yet.
    #[default]
    NotLoaded,
    Loading,
    Loaded(DiskUsage),
    Failed(String),
}

const VM_OFF: &str = "The WSL VM is not running.";
const NEEDS_TWO_SAMPLES: &str = "Measured between two samples; the second one is on its way.";

/// The VM's figures, or the meter to show for `kind` when there are none:
/// no sample yet, a sample that failed, a VM that is off or that cannot be
/// told apart.
fn without_vm(kind: MeterKind, host: Option<&HostReading>) -> Result<&VmUsage, Meter> {
    let meter = |state, value, detail: String| Meter::new(kind, state, value, detail);
    match host {
        None => Err(meter(
            MeterState::Measuring,
            UNKNOWN,
            "The first sample is on its way.".to_string(),
        )),
        Some(HostReading::Failed(reason)) => {
            Err(meter(MeterState::Unknown, UNKNOWN, reason.clone()))
        }
        Some(HostReading::Sample(usage)) => match &usage.vm {
            VmStatus::Found(vm) => Ok(vm),
            VmStatus::NotRunning => Err(meter(MeterState::Off, "Off", VM_OFF.to_string())),
            VmStatus::Ambiguous { candidates } => Err(meter(
                MeterState::Unknown,
                UNKNOWN,
                format!(
                    "{candidates} virtual machines are running and the one of WSL cannot be \
                     told apart from the others."
                ),
            )),
        },
    }
}

fn sample(host: Option<&HostReading>) -> Option<&HostUsage> {
    match host {
        Some(HostReading::Sample(usage)) => Some(usage),
        _ => None,
    }
}

/// Memory of the WSL VM as Task Manager shows it. `containers` adds how
/// much of it the containers account for.
pub fn memory_meter(host: Option<&HostReading>, containers: Option<&[ContainerUsage]>) -> Meter {
    let vm = match without_vm(MeterKind::Memory, host) {
        Ok(vm) => vm,
        Err(meter) => return meter,
    };
    // Older Windows reports no private working set for the VM process.
    let bytes = match vm.private_working_set_bytes {
        0 => vm.working_set_bytes,
        private => private,
    };
    let mut detail = format!(
        "Memory of the WSL VM ({}), which all WSL distros share.",
        vm.name
    );
    if let Some(containers) = containers {
        let used: u64 = containers.iter().map(|usage| usage.memory_used).sum();
        detail.push_str(&format!(" Containers use {}.", format_bytes(used)));
    }
    Meter::known(MeterKind::Memory, format_bytes(bytes), detail)
}

/// CPU of the WSL VM in percent of the whole machine.
pub fn cpu_meter(host: Option<&HostReading>, containers: Option<&[ContainerUsage]>) -> Meter {
    let vm = match without_vm(MeterKind::Cpu, host) {
        Ok(vm) => vm,
        Err(meter) => return meter,
    };
    let Some(percent) = vm.cpu_percent else {
        return Meter::new(
            MeterKind::Cpu,
            MeterState::Measuring,
            UNKNOWN,
            NEEDS_TWO_SAMPLES,
        );
    };
    let mut detail = "CPU of the WSL VM, in percent of all processors.".to_string();
    let by_containers: Option<f64> = containers.map(|containers| {
        let known = containers.iter().filter_map(|usage| usage.cpu_percent);
        known.sum()
    });
    if let Some(by_containers) = by_containers {
        detail.push_str(&format!(
            " Containers use {} of one processor.",
            format_percent(by_containers)
        ));
    }
    Meter::known(MeterKind::Cpu, format!("{percent:.0}%"), detail).filled(percent / 100.0)
}

/// GPU attributed to the WSL VM.
pub fn gpu_meter(host: Option<&HostReading>) -> Meter {
    if let Err(meter) = without_vm(MeterKind::Gpu, host) {
        return meter;
    }
    let Some(gpu) = sample(host).and_then(|usage| usage.gpu) else {
        return Meter::new(
            MeterKind::Gpu,
            MeterState::Unknown,
            UNKNOWN,
            "Windows publishes no GPU counters for the WSL VM.",
        );
    };
    let memory = format!(
        "GPU memory of the WSL VM: {} dedicated, {} shared.",
        format_bytes(gpu.dedicated_bytes),
        format_bytes(gpu.shared_bytes)
    );
    match gpu.utilization_percent {
        Some(percent) => {
            Meter::known(MeterKind::Gpu, format!("{percent:.0}%"), memory).filled(percent / 100.0)
        }
        None => Meter::new(
            MeterKind::Gpu,
            MeterState::Measuring,
            UNKNOWN,
            format!("{NEEDS_TWO_SAMPLES} {memory}"),
        ),
    }
}

/// The size of the distro's virtual disk, against the free space of the
/// drive it is on. It is there whether the VM runs or not.
pub fn disk_meter(host: Option<&HostReading>) -> Meter {
    let usage = match host {
        Some(HostReading::Sample(usage)) => usage,
        Some(HostReading::Failed(reason)) => {
            return Meter::new(
                MeterKind::Disk,
                MeterState::Unknown,
                UNKNOWN,
                reason.clone(),
            );
        }
        None => {
            return Meter::new(
                MeterKind::Disk,
                MeterState::Measuring,
                UNKNOWN,
                "The first sample is on its way.",
            );
        }
    };
    let Some(size) = usage.disk.vhdx_bytes else {
        return Meter::new(
            MeterKind::Disk,
            MeterState::Unknown,
            UNKNOWN,
            "The engine's virtual disk was not found; it appears with the first install.",
        );
    };
    let what = "Size of the engine's virtual disk.";
    let meter = |detail: String| Meter::known(MeterKind::Disk, format_bytes(size), detail);
    match usage.disk.free_bytes {
        Some(free) => {
            let detail = format!("{what} {} are free on its drive.", format_bytes(free));
            let whole = size.saturating_add(free);
            if whole == 0 {
                meter(detail)
            } else {
                meter(detail).filled(size as f64 / whole as f64)
            }
        }
        None => meter(what.to_string()),
    }
}

/// What Docker keeps on the disk, and how much of it a prune would free.
/// The bar shows the part that is in use.
pub fn docker_data_meter(disk: &DiskReading) -> Meter {
    let kind = MeterKind::DockerData;
    let usage = match disk {
        DiskReading::Loaded(usage) => usage,
        DiskReading::NotLoaded => {
            return Meter::new(
                kind,
                MeterState::NotMeasured,
                UNKNOWN,
                "Not measured yet. Docker works it out when it is asked for.",
            );
        }
        DiskReading::Loading => {
            return Meter::new(
                kind,
                MeterState::Measuring,
                UNKNOWN,
                "Docker is working it out.",
            );
        }
        DiskReading::Failed(reason) => {
            return Meter::new(kind, MeterState::Unknown, UNKNOWN, reason.clone());
        }
    };
    let size = usage.total_size();
    let reclaimable = usage.total_reclaimable();
    let detail = format!(
        "{} can be reclaimed. Images {}, containers {}, volumes {}, build cache {}.",
        format_bytes(reclaimable),
        format_bytes(usage.image_totals.size),
        format_bytes(usage.container_totals.size),
        format_bytes(usage.volume_totals.size),
        format_bytes(usage.build_cache_totals.size)
    );
    let meter = Meter::known(kind, format_bytes(size), detail);
    if size == 0 {
        meter
    } else {
        meter.filled(size.saturating_sub(reclaimable) as f64 / size as f64)
    }
}

/// Everything the usage strip and the usage columns show. Lives on the UI
/// thread; the figures arrive as messages.
#[derive(Debug, Default)]
pub struct UsageModel {
    host: Option<HostReading>,
    containers: Option<Vec<ContainerUsage>>,
    disk: DiskReading,
    generation: u64,
}

impl UsageModel {
    /// Changes whenever a figure may have changed.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn set_host(&mut self, reading: HostReading) {
        self.host = Some(reading);
        self.generation += 1;
    }

    /// The usage of the running containers, or `None` when it stopped
    /// coming.
    pub fn set_containers(&mut self, usage: Option<Vec<ContainerUsage>>) {
        self.containers = usage;
        self.generation += 1;
    }

    /// For `Table::apply_usage` and `ListModel::set_usage`.
    pub fn containers(&self) -> Option<&[ContainerUsage]> {
        self.containers.as_deref()
    }

    /// Docker's disk usage was asked for.
    pub fn disk_loading(&mut self) {
        self.disk = DiskReading::Loading;
        self.generation += 1;
    }

    pub fn set_disk(&mut self, result: Result<DiskUsage, String>) {
        self.disk = match result {
            Ok(usage) => DiskReading::Loaded(usage),
            Err(reason) => DiskReading::Failed(reason),
        };
        self.generation += 1;
    }

    /// Docker's disk usage, if it was loaded. For `volumes_table`.
    pub fn disk(&self) -> Option<&DiskUsage> {
        match &self.disk {
            DiskReading::Loaded(usage) => Some(usage),
            _ => None,
        }
    }

    /// True while Docker's disk usage is being worked out.
    pub fn disk_is_loading(&self) -> bool {
        self.disk == DiskReading::Loading
    }

    /// Forgets Docker's disk usage after something was removed: the figures
    /// are from before, and they are only worked out again when asked for.
    pub fn forget_disk(&mut self) {
        if self.disk != DiskReading::NotLoaded {
            self.disk = DiskReading::NotLoaded;
            self.generation += 1;
        }
    }

    /// Forgets what only a running engine can say: the containers' usage
    /// and Docker's disk usage. The host's figures stay.
    pub fn engine_left(&mut self) {
        self.containers = None;
        self.disk = DiskReading::NotLoaded;
        self.generation += 1;
    }

    /// Forgets everything, when the sampling ended with the window hidden:
    /// figures from before would look current when it shows again.
    pub fn clear(&mut self) {
        *self = Self {
            generation: self.generation + 1,
            ..Self::default()
        };
    }

    /// The strip: memory, CPU, GPU, disk, Docker's data.
    pub fn meters(&self) -> Vec<Meter> {
        let host = self.host.as_ref();
        let containers = self.containers();
        vec![
            memory_meter(host, containers),
            cpu_meter(host, containers),
            gpu_meter(host),
            disk_meter(host),
            docker_data_meter(&self.disk),
        ]
    }
}

#[cfg(test)]
mod tests {
    use docklet_engine::DiskTotals;
    use docklet_wsl::usage::{DiskSpace, DockletProcesses, GpuUsage};

    use super::*;
    use crate::table::testing::usage;

    fn vm(cpu_percent: Option<f64>) -> VmStatus {
        VmStatus::Found(VmUsage {
            pid: 4321,
            name: "vmmemWSL".to_string(),
            working_set_bytes: 3_400_000_000,
            private_working_set_bytes: 3_100_000_000,
            cpu_percent,
        })
    }

    fn reading(vm: VmStatus, gpu: Option<GpuUsage>) -> HostReading {
        HostReading::Sample(HostUsage {
            vm,
            gpu,
            disk: DiskSpace {
                vhdx_bytes: Some(12_000_000_000),
                free_bytes: Some(36_000_000_000),
            },
            docklet: DockletProcesses::default(),
        })
    }

    fn gpu(utilization_percent: Option<f64>) -> GpuUsage {
        GpuUsage {
            utilization_percent,
            dedicated_bytes: 512_000_000,
            shared_bytes: 64_000_000,
        }
    }

    #[test]
    fn a_found_vm_shows_its_memory_and_cpu() {
        let host = reading(vm(Some(12.4)), None);
        let memory = memory_meter(Some(&host), None);
        assert_eq!(memory.label, "Memory");
        assert_eq!(memory.value, "3.1 GB");
        assert_eq!(memory.state, MeterState::Known);
        assert_eq!(memory.fraction, None);
        assert!(memory.detail.contains("vmmemWSL"));

        let cpu = cpu_meter(Some(&host), None);
        assert_eq!(cpu.value, "12%");
        assert_eq!(cpu.state, MeterState::Known);
        assert!((cpu.fraction.unwrap() - 0.124).abs() < 1e-9);
    }

    #[test]
    fn the_containers_share_is_added_when_it_is_known() {
        let host = reading(vm(Some(10.0)), None);
        let containers = [
            usage("a", Some(40.0), 1_000_000_000),
            usage("b", None, 200_000_000),
        ];
        let memory = memory_meter(Some(&host), Some(&containers));
        assert!(memory.detail.ends_with("Containers use 1.2 GB."));
        let cpu = cpu_meter(Some(&host), Some(&containers));
        assert!(
            cpu.detail
                .ends_with("Containers use 40.0% of one processor.")
        );
    }

    #[test]
    fn memory_falls_back_to_the_working_set() {
        let found = VmStatus::Found(VmUsage {
            pid: 1,
            name: "vmmem".to_string(),
            working_set_bytes: 2_000_000_000,
            private_working_set_bytes: 0,
            cpu_percent: None,
        });
        let memory = memory_meter(Some(&reading(found, None)), None);
        assert_eq!(memory.value, "2.0 GB");
    }

    #[test]
    fn the_first_cpu_sample_is_still_measuring() {
        let host = reading(vm(None), None);
        let cpu = cpu_meter(Some(&host), None);
        assert_eq!(cpu.state, MeterState::Measuring);
        assert_eq!(cpu.value, UNKNOWN);
        assert_eq!(cpu.fraction, None);
        // Memory needs no second sample.
        assert_eq!(memory_meter(Some(&host), None).state, MeterState::Known);
    }

    #[test]
    fn before_the_first_sample_everything_from_the_host_is_measuring() {
        for meter in [
            memory_meter(None, None),
            cpu_meter(None, None),
            gpu_meter(None),
            disk_meter(None),
        ] {
            assert_eq!(meter.state, MeterState::Measuring, "{:?}", meter.kind);
            assert_eq!(meter.value, UNKNOWN);
        }
    }

    #[test]
    fn a_vm_that_does_not_run_is_off_not_unknown() {
        let host = reading(VmStatus::NotRunning, None);
        for meter in [
            memory_meter(Some(&host), None),
            cpu_meter(Some(&host), None),
            gpu_meter(Some(&host)),
        ] {
            assert_eq!(meter.state, MeterState::Off, "{:?}", meter.kind);
            assert_eq!(meter.value, "Off");
        }
        // The disk is there all the same.
        assert_eq!(disk_meter(Some(&host)).state, MeterState::Known);
    }

    #[test]
    fn a_vm_that_cannot_be_told_apart_gives_no_figures() {
        let host = reading(VmStatus::Ambiguous { candidates: 2 }, None);
        let memory = memory_meter(Some(&host), None);
        assert_eq!(memory.state, MeterState::Unknown);
        assert_eq!(memory.value, UNKNOWN);
        assert!(memory.detail.starts_with("2 virtual machines"));
        assert_eq!(cpu_meter(Some(&host), None).state, MeterState::Unknown);
        assert_eq!(gpu_meter(Some(&host)).state, MeterState::Unknown);
    }

    #[test]
    fn missing_gpu_counters_are_unknown() {
        let host = reading(vm(Some(1.0)), None);
        let meter = gpu_meter(Some(&host));
        assert_eq!(meter.state, MeterState::Unknown);
        assert_eq!(meter.value, UNKNOWN);
        assert!(meter.detail.contains("no GPU counters"));
    }

    #[test]
    fn gpu_shows_utilisation_once_two_samples_were_taken() {
        let first = gpu_meter(Some(&reading(vm(Some(1.0)), Some(gpu(None)))));
        assert_eq!(first.state, MeterState::Measuring);
        // Its memory is known from the first sample on.
        assert!(first.detail.contains("512 MB dedicated, 64.0 MB shared"));

        let second = gpu_meter(Some(&reading(vm(Some(1.0)), Some(gpu(Some(37.6))))));
        assert_eq!(second.state, MeterState::Known);
        assert_eq!(second.value, "38%");
        assert!((second.fraction.unwrap() - 0.376).abs() < 1e-9);
    }

    #[test]
    fn a_sample_that_failed_says_why_on_every_host_meter() {
        let host = HostReading::Failed("the process list could not be read".to_string());
        for meter in [
            memory_meter(Some(&host), None),
            cpu_meter(Some(&host), None),
            gpu_meter(Some(&host)),
            disk_meter(Some(&host)),
        ] {
            assert_eq!(meter.state, MeterState::Unknown, "{:?}", meter.kind);
            assert_eq!(meter.detail, "the process list could not be read");
        }
    }

    #[test]
    fn the_disk_is_drawn_against_the_free_space_of_its_drive() {
        let meter = disk_meter(Some(&reading(VmStatus::NotRunning, None)));
        assert_eq!(meter.value, "12.0 GB");
        assert!(meter.detail.contains("36.0 GB are free"));
        assert!((meter.fraction.unwrap() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn a_disk_that_is_not_there_or_has_no_free_space_figure() {
        let with_disk = |disk| {
            HostReading::Sample(HostUsage {
                vm: VmStatus::NotRunning,
                gpu: None,
                disk,
                docklet: DockletProcesses::default(),
            })
        };
        let missing = disk_meter(Some(&with_disk(DiskSpace::default())));
        assert_eq!(missing.state, MeterState::Unknown);
        assert_eq!(missing.value, UNKNOWN);

        let no_free = disk_meter(Some(&with_disk(DiskSpace {
            vhdx_bytes: Some(5_000_000_000),
            free_bytes: None,
        })));
        assert_eq!(no_free.state, MeterState::Known);
        assert_eq!(no_free.value, "5.0 GB");
        assert_eq!(no_free.fraction, None);
    }

    fn docker_disk() -> DiskUsage {
        let totals = |size, reclaimable| DiskTotals {
            count: 1,
            active: 1,
            size,
            reclaimable,
        };
        DiskUsage {
            image_totals: totals(6_000_000_000, 2_000_000_000),
            container_totals: totals(500_000_000, 0),
            volume_totals: totals(1_000_000_000, 0),
            build_cache_totals: totals(500_000_000, 0),
            ..Default::default()
        }
    }

    #[test]
    fn docker_data_is_unknown_until_it_is_asked_for() {
        let not_loaded = docker_data_meter(&DiskReading::NotLoaded);
        assert_eq!(not_loaded.state, MeterState::NotMeasured);
        assert_eq!(not_loaded.value, UNKNOWN);
        let loading = docker_data_meter(&DiskReading::Loading);
        assert_eq!(loading.state, MeterState::Measuring);
        let failed = docker_data_meter(&DiskReading::Failed("timeout".to_string()));
        assert_eq!(failed.state, MeterState::Unknown);
        assert_eq!(failed.detail, "timeout");
    }

    #[test]
    fn docker_data_shows_its_size_and_what_can_be_reclaimed() {
        let meter = docker_data_meter(&DiskReading::Loaded(docker_disk()));
        assert_eq!(meter.label, "Docker data");
        assert_eq!(meter.value, "8.0 GB");
        assert!(meter.detail.starts_with("2.0 GB can be reclaimed."));
        assert!(meter.detail.contains("Images 6.0 GB"));
        assert!((meter.fraction.unwrap() - 0.75).abs() < 1e-9);
        // Nothing stored: nothing to draw.
        let empty = docker_data_meter(&DiskReading::Loaded(DiskUsage::default()));
        assert_eq!(empty.value, "0 B");
        assert_eq!(empty.fraction, None);
    }

    #[test]
    fn the_strip_has_five_meters_in_a_fixed_order() {
        let model = UsageModel::default();
        let kinds: Vec<MeterKind> = model.meters().iter().map(|meter| meter.kind).collect();
        assert_eq!(
            kinds,
            [
                MeterKind::Memory,
                MeterKind::Cpu,
                MeterKind::Gpu,
                MeterKind::Disk,
                MeterKind::DockerData,
            ]
        );
    }

    #[test]
    fn the_model_keeps_what_arrives_and_counts_the_changes() {
        let mut model = UsageModel::default();
        let start = model.generation();
        model.set_host(reading(vm(Some(5.0)), None));
        model.set_containers(Some(vec![usage("a", Some(1.0), 1_000_000)]));
        model.disk_loading();
        assert_eq!(model.meters()[4].state, MeterState::Measuring);
        model.set_disk(Ok(docker_disk()));
        assert_eq!(model.generation(), start + 4);
        assert_eq!(model.containers().map(<[_]>::len), Some(1));
        assert!(model.disk().is_some());
        assert_eq!(model.meters()[0].value, "3.1 GB");
        assert_eq!(model.meters()[4].value, "8.0 GB");

        model.set_disk(Err("no answer".to_string()));
        assert!(model.disk().is_none());
        assert_eq!(model.meters()[4].detail, "no answer");
    }

    #[test]
    fn an_engine_that_left_takes_its_figures_along_but_not_the_hosts() {
        let mut model = UsageModel::default();
        model.set_host(reading(VmStatus::NotRunning, None));
        model.set_containers(Some(Vec::new()));
        model.set_disk(Ok(docker_disk()));
        model.engine_left();
        assert!(model.containers().is_none());
        assert!(model.disk().is_none());
        assert_eq!(model.meters()[0].state, MeterState::Off);
        assert_eq!(model.meters()[4].state, MeterState::NotMeasured);
    }

    #[test]
    fn disk_figures_from_before_a_removal_are_forgotten() {
        let mut model = UsageModel::default();
        model.disk_loading();
        assert!(model.disk_is_loading());
        model.set_disk(Ok(docker_disk()));
        assert!(!model.disk_is_loading());
        let before = model.generation();
        model.forget_disk();
        assert!(model.disk().is_none());
        assert_eq!(model.generation(), before + 1);
        // Nothing to forget: nothing changed.
        model.forget_disk();
        assert_eq!(model.generation(), before + 1);
    }

    #[test]
    fn clearing_goes_back_to_measuring() {
        let mut model = UsageModel::default();
        model.set_host(reading(vm(Some(5.0)), None));
        let before = model.generation();
        model.clear();
        assert!(model.generation() > before);
        assert_eq!(model.meters()[0].state, MeterState::Measuring);
    }
}
