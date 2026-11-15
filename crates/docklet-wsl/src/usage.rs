//! What the engine costs as Windows sees it: memory and CPU of the WSL VM,
//! GPU attributed to it, the distro's disk, and Docklet's own processes.
//!
//! Everything is read-only and nothing here starts `wsl.exe`, so taking a
//! sample neither starts a stopped engine nor wakes a sleeping one. One
//! sample costs a system process list, a few GPU performance counters and a
//! file size: cheap enough to take every couple of seconds while a window
//! shows it.
//!
//! The WSL VM is shared by every WSL 2 distro of the user, so its memory,
//! CPU and GPU include any other distro that runs at the same time.
//!
//! The pure parts (picking the VM process, CPU percent, Docklet's process
//! set, the GPU counter folding) build and test everywhere; reading them
//! from Windows is behind `cfg(windows)`, and elsewhere `UsageSampler::sample`
//! fails with `ErrorKind::Unsupported`.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use docklet_core::engine::DISTRO_NAME;

/// What the engine costs, at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct HostUsage {
    pub vm: VmStatus,
    /// GPU used by the VM process. `None` when the counters are missing
    /// (no GPU driver that publishes them) or the VM process is not known.
    pub gpu: Option<GpuUsage>,
    pub disk: DiskSpace,
    pub docklet: DockletProcesses,
}

/// The WSL VM process, if it can be told apart.
#[derive(Debug, Clone, PartialEq)]
pub enum VmStatus {
    /// No VM process: WSL, and so the engine, is not running.
    NotRunning,
    Found(VmUsage),
    /// Only processes named `vmmem` exist, which Hyper-V VMs use too, and
    /// nothing tells which one is WSL's. No figures are given rather than
    /// another VM's.
    Ambiguous {
        candidates: usize,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct VmUsage {
    pub pid: u32,
    /// `vmmemWSL`, or `vmmem` on older WSL.
    pub name: String,
    /// Memory of the VM resident in RAM.
    pub working_set_bytes: u64,
    /// The part of the working set no other process shares; what Task
    /// Manager shows in its Memory column.
    pub private_working_set_bytes: u64,
    /// CPU since the previous sample in percent of the whole machine (all
    /// logical processors), as Task Manager shows it. `None` for the first
    /// sample and when the VM restarted in between.
    pub cpu_percent: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpuUsage {
    /// The busiest GPU engine's utilisation by the VM, as Task Manager's GPU
    /// column. `None` until two samples have been taken.
    pub utilization_percent: Option<f64>,
    /// GPU memory on the card attributed to the VM.
    pub dedicated_bytes: u64,
    /// System memory the GPU uses for the VM.
    pub shared_bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskSpace {
    /// Size of the distro's `ext4.vhdx`; `None` when it is not there.
    pub vhdx_bytes: Option<u64>,
    /// Free space for the user on the drive that holds it.
    pub free_bytes: Option<u64>,
}

/// Docklet's own processes: `docklet-desktop.exe` with its WebView, the
/// `docklet.exe` proxy and CLI, and the `wsl.exe` relays and keepalive that
/// serve the docklet distro.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DockletProcesses {
    pub count: usize,
    pub working_set_bytes: u64,
    /// How many of them are `wsl.exe` processes for the docklet distro.
    pub wsl_clients: usize,
}

/// One entry of the system's process list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub parent_pid: u32,
    /// The image name, such as `wsl.exe` or `vmmemWSL`.
    pub name: String,
    /// When the process started, in 100 ns units (a `FILETIME`).
    pub create_time: i64,
    /// User plus kernel time, in 100 ns units.
    pub cpu_time: u64,
    pub working_set: u64,
    pub private_working_set: u64,
}

impl ProcessInfo {
    /// Compares image names the way Windows does: ignoring case, and with
    /// or without `.exe` (`vmmemWSL` has none).
    fn is_named(&self, name: &str) -> bool {
        without_exe(&self.name).eq_ignore_ascii_case(without_exe(name))
    }
}

fn without_exe(name: &str) -> &str {
    let split = name
        .len()
        .checked_sub(4)
        .filter(|&at| name.is_char_boundary(at));
    match split {
        Some(at) if name[at..].eq_ignore_ascii_case(".exe") => &name[..at],
        _ => name,
    }
}

/// Which process is the WSL VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmPick<'a> {
    NotRunning,
    Found(&'a ProcessInfo),
    Ambiguous { candidates: usize },
}

/// Finds the WSL VM among the processes.
///
/// Current WSL names its VM process `vmmemWSL`, which is what tells it from
/// Hyper-V VMs, whose processes are named `vmmem`. A single `vmmemWSL` is the
/// answer. Older WSL used `vmmem` too; then a single `vmmem` is taken as
/// WSL's only when `wsl_running` (a `wsl.exe` for the docklet distro exists,
/// so the WSL VM must be up). Any other mix is ambiguous: two `vmmemWSL`
/// (two users run WSL), several `vmmem`, or one with no sign of WSL.
pub fn pick_vm_process(processes: &[ProcessInfo], wsl_running: bool) -> VmPick<'_> {
    let wsl: Vec<_> = processes
        .iter()
        .filter(|p| p.is_named("vmmemWSL"))
        .collect();
    match wsl.as_slice() {
        [only] => return VmPick::Found(only),
        [] => {}
        several => {
            return VmPick::Ambiguous {
                candidates: several.len(),
            };
        }
    }
    let legacy: Vec<_> = processes.iter().filter(|p| p.is_named("vmmem")).collect();
    match legacy.as_slice() {
        [] => VmPick::NotRunning,
        [only] if wsl_running => VmPick::Found(only),
        several => VmPick::Ambiguous {
            candidates: several.len(),
        },
    }
}

/// The VM's CPU counter at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuSample {
    pub pid: u32,
    pub create_time: i64,
    /// User plus kernel time, in 100 ns units.
    pub cpu_time: u64,
    pub taken: Instant,
}

/// CPU used between two samples of the same process, in percent of all
/// `logical_cpus`. `None` when the samples are of different processes (the
/// VM restarted, or a PID was reused), no time passed, or the counter went
/// back.
pub fn cpu_percent(previous: &CpuSample, current: &CpuSample, logical_cpus: u32) -> Option<f64> {
    let same = previous.pid == current.pid && previous.create_time == current.create_time;
    let elapsed = current.taken.checked_duration_since(previous.taken)?;
    let used = current.cpu_time.checked_sub(previous.cpu_time)?;
    if !same || elapsed.is_zero() || logical_cpus == 0 {
        return None;
    }
    // CPU time is in 100 ns units.
    let available = elapsed.as_secs_f64() * 1e7 * f64::from(logical_cpus);
    Some((used as f64 / available * 100.0).clamp(0.0, 100.0))
}

/// The image names of Docklet's own programs.
const DOCKLET_PROGRAMS: [&str; 2] = ["docklet.exe", "docklet-desktop.exe"];

/// Docklet's processes: its programs, everything they started (the WebView
/// of the window, relays, consoles), and `wsl.exe` processes that
/// `is_docklet_wsl` recognises, which catches the keepalive after the
/// `docklet start` that spawned it has exited.
///
/// A child counts only if it started after its parent: Windows reuses PIDs,
/// and an old process may name a parent PID that now belongs to Docklet.
pub fn docklet_processes(
    processes: &[ProcessInfo],
    mut is_docklet_wsl: impl FnMut(&ProcessInfo) -> bool,
) -> Vec<&ProcessInfo> {
    let mut members: Vec<bool> = processes
        .iter()
        .map(|p| DOCKLET_PROGRAMS.iter().any(|name| p.is_named(name)))
        .collect();
    let by_pid: HashMap<u32, usize> = processes
        .iter()
        .enumerate()
        .map(|(index, process)| (process.pid, index))
        .collect();
    // Grow the set until nothing changes. Each round adds at least one
    // process or ends, so this ends.
    loop {
        let mut grew = false;
        for (index, process) in processes.iter().enumerate() {
            if members[index] {
                continue;
            }
            let parent = by_pid
                .get(&process.parent_pid)
                .copied()
                .filter(|&parent| parent != index);
            if let Some(parent) = parent
                && members[parent]
                && processes[parent].create_time <= process.create_time
            {
                members[index] = true;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    for (index, process) in processes.iter().enumerate() {
        if !members[index] && process.is_named("wsl.exe") && is_docklet_wsl(process) {
            members[index] = true;
        }
    }
    processes
        .iter()
        .zip(members)
        .filter_map(|(process, member)| member.then_some(process))
        .collect()
}

/// Sums up Docklet's processes.
pub fn docklet_totals(members: &[&ProcessInfo]) -> DockletProcesses {
    DockletProcesses {
        count: members.len(),
        working_set_bytes: members.iter().map(|p| p.working_set).sum(),
        wsl_clients: members.iter().filter(|p| p.is_named("wsl.exe")).count(),
    }
}

/// True when a `wsl.exe` command line runs something in the docklet distro:
/// `-d docklet` or `--distribution docklet`.
pub fn names_docklet_distro(command_line: &str) -> bool {
    let mut words = command_line
        .split_whitespace()
        .map(|word| word.trim_matches('"'));
    while let Some(word) = words.next() {
        if matches!(word, "-d" | "--distribution")
            && words
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(DISTRO_NAME))
        {
            return true;
        }
    }
    false
}

/// The process ID in a GPU counter instance name, such as
/// `pid_1234_luid_0x00000000_0x0000D1F2_phys_0_eng_3_engtype_VideoDecode`
/// (`GPU Engine`) or `pid_1234_luid_0x00000000_0x0000D1F2_phys_0`
/// (`GPU Process Memory`).
pub fn gpu_instance_pid(instance: &str) -> Option<u32> {
    let rest = instance.strip_prefix("pid_")?;
    let digits = rest.split('_').next()?;
    digits.parse().ok()
}

/// One process's GPU use from the three counter arrays. Utilisation is the
/// busiest engine's (as Task Manager shows it); memory is summed over the
/// adapters. `engines` is `None` while the rate counter has no value yet.
pub fn gpu_for_process(
    pid: u32,
    engines: Option<&[(String, f64)]>,
    dedicated: &[(String, f64)],
    shared: &[(String, f64)],
) -> GpuUsage {
    let of_pid = |items: &[(String, f64)]| {
        items
            .iter()
            .filter(|(instance, _)| gpu_instance_pid(instance) == Some(pid))
            .map(|(_, value)| *value)
            .filter(|value| value.is_finite() && *value > 0.0)
            .collect::<Vec<_>>()
    };
    let bytes = |items: &[(String, f64)]| of_pid(items).iter().sum::<f64>() as u64;
    GpuUsage {
        utilization_percent: engines.map(|engines| {
            of_pid(engines)
                .into_iter()
                .fold(0.0, f64::max)
                .clamp(0.0, 100.0)
        }),
        dedicated_bytes: bytes(dedicated),
        shared_bytes: bytes(shared),
    }
}

/// The distro's virtual disk, under Docklet's data directory.
pub fn vhdx_path(data_dir: &Path) -> PathBuf {
    data_dir.join("distro").join("ext4.vhdx")
}

/// Takes samples of `HostUsage`, keeping what CPU and GPU rates need from
/// one sample to the next. Create one per window and keep it; a fresh
/// sampler cannot give CPU or GPU percentages in its first sample.
pub struct UsageSampler {
    data_dir: PathBuf,
    previous_cpu: Option<CpuSample>,
    #[cfg(windows)]
    gpu: Option<windows_impl::GpuCounters>,
}

impl UsageSampler {
    /// A sampler for the distro under `data_dir` (see `crate::data_dir`).
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            previous_cpu: None,
            #[cfg(windows)]
            gpu: windows_impl::GpuCounters::open(),
        }
    }

    /// A sampler for Docklet's own data directory.
    pub fn system() -> io::Result<Self> {
        Ok(Self::new(crate::data_dir()?))
    }

    /// The engine's cost now. Fails only if the process list cannot be read;
    /// a missing disk or GPU counter shows up as `None` in its field.
    #[cfg(windows)]
    pub fn sample(&mut self) -> io::Result<HostUsage> {
        let processes = windows_impl::process_list()?;
        let taken = Instant::now();
        let members = docklet_processes(&processes, |process| {
            windows_impl::command_line(process.pid).is_some_and(|line| names_docklet_distro(&line))
        });
        let docklet = docklet_totals(&members);

        let (vm, gpu) = match pick_vm_process(&processes, docklet.wsl_clients > 0) {
            VmPick::NotRunning => {
                self.previous_cpu = None;
                (VmStatus::NotRunning, None)
            }
            VmPick::Ambiguous { candidates } => {
                self.previous_cpu = None;
                (VmStatus::Ambiguous { candidates }, None)
            }
            VmPick::Found(process) => {
                let current = CpuSample {
                    pid: process.pid,
                    create_time: process.create_time,
                    cpu_time: process.cpu_time,
                    taken,
                };
                let cpus = windows_impl::logical_cpus();
                let cpu = self
                    .previous_cpu
                    .and_then(|previous| cpu_percent(&previous, &current, cpus));
                self.previous_cpu = Some(current);
                let gpu = self.gpu.as_mut().and_then(|gpu| gpu.read(process.pid));
                let vm = VmUsage {
                    pid: process.pid,
                    name: process.name.clone(),
                    working_set_bytes: process.working_set,
                    private_working_set_bytes: process.private_working_set,
                    cpu_percent: cpu,
                };
                (VmStatus::Found(vm), gpu)
            }
        };

        let vhdx = vhdx_path(&self.data_dir);
        let disk = DiskSpace {
            vhdx_bytes: std::fs::metadata(&vhdx).ok().map(|meta| meta.len()),
            free_bytes: docklet_core::disk::free_space(&vhdx).ok(),
        };
        Ok(HostUsage {
            vm,
            gpu,
            disk,
            docklet,
        })
    }

    /// The engine's cost is only measured on Windows.
    #[cfg(not(windows))]
    pub fn sample(&mut self) -> io::Result<HostUsage> {
        let _ = (&self.data_dir, self.previous_cpu);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "host resource usage is only measured on Windows",
        ))
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::io;

    use windows::Wdk::System::SystemInformation::{
        NtQuerySystemInformation, SystemProcessInformation,
    };
    use windows::Wdk::System::Threading::{
        NtQueryInformationProcess, ProcessCommandLineInformation,
    };
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Performance::{
        PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
        PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery,
        PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
    };
    use windows::Win32::System::Threading::{
        ALL_PROCESSOR_GROUPS, GetActiveProcessorCount, OpenProcess,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::core::{PCWSTR, w};

    use super::{GpuUsage, ProcessInfo, gpu_for_process};

    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
    const STATUS_BUFFER_TOO_SMALL: i32 = 0xC000_0023_u32 as i32;
    const STATUS_BUFFER_OVERFLOW: i32 = 0x8000_0005_u32 as i32;

    /// `UNICODE_STRING`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *const u16,
    }

    /// The start of `SYSTEM_PROCESS_INFORMATION`, with the fields the
    /// Windows SDK calls `Reserved` named as the NT headers name them. The
    /// layout has been stable since Windows 7.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcessEntry {
        next_entry_offset: u32,
        number_of_threads: u32,
        working_set_private_size: i64,
        hard_fault_count: u32,
        number_of_threads_high_watermark: u32,
        cycle_time: u64,
        create_time: i64,
        user_time: i64,
        kernel_time: i64,
        image_name: UnicodeString,
        base_priority: i32,
        unique_process_id: usize,
        inherited_from_unique_process_id: usize,
        handle_count: u32,
        session_id: u32,
        unique_process_key: usize,
        peak_virtual_size: usize,
        virtual_size: usize,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
    }

    /// Every process on the system, from one `NtQuerySystemInformation`
    /// call. Unlike opening each process, this also covers the VM process,
    /// which a normal user may not open.
    pub(super) fn process_list() -> io::Result<Vec<ProcessInfo>> {
        // `u64` elements keep the entries' 8-byte fields aligned.
        let mut buffer = vec![0u64; 64 * 1024];
        // The list grows between the size query and the call when processes
        // start; a few attempts with room to spare are enough.
        for _ in 0..4 {
            let bytes = u32::try_from(buffer.len() * 8).unwrap_or(u32::MAX);
            let mut needed = 0u32;
            // SAFETY: the buffer is writable for `bytes` bytes and `needed`
            // is a valid `u32` to write to.
            let status = unsafe {
                NtQuerySystemInformation(
                    SystemProcessInformation,
                    buffer.as_mut_ptr().cast(),
                    bytes,
                    &mut needed,
                )
            };
            if status.0 == STATUS_INFO_LENGTH_MISMATCH {
                let words = (needed as usize).max(bytes as usize * 2) / 8 + 8 * 1024;
                buffer = vec![0u64; words];
                continue;
            }
            if status.is_err() {
                return Err(io::Error::other(format!(
                    "the process list could not be read (NTSTATUS {:#010x})",
                    status.0
                )));
            }
            return Ok(parse_process_list(&buffer));
        }
        Err(io::Error::other(
            "the process list kept growing while it was read",
        ))
    }

    fn parse_process_list(buffer: &[u64]) -> Vec<ProcessInfo> {
        let base = buffer.as_ptr().cast::<u8>();
        let len = buffer.len() * 8;
        let mut processes = Vec::new();
        let mut offset = 0usize;
        while offset + size_of::<ProcessEntry>() <= len {
            // SAFETY: the entry lies within the buffer (checked above) and
            // the kernel wrote it there; it is read unaligned to need no
            // assumption about the offsets.
            let entry = unsafe { base.add(offset).cast::<ProcessEntry>().read_unaligned() };
            let name = image_name(entry.image_name);
            processes.push(ProcessInfo {
                pid: entry.unique_process_id as u32,
                parent_pid: entry.inherited_from_unique_process_id as u32,
                // The idle process has no name.
                name: if entry.unique_process_id == 0 {
                    "Idle".to_string()
                } else {
                    name
                },
                create_time: entry.create_time,
                cpu_time: (entry.user_time.max(0) + entry.kernel_time.max(0)) as u64,
                working_set: entry.working_set_size as u64,
                private_working_set: entry.working_set_private_size.max(0) as u64,
            });
            if entry.next_entry_offset == 0 {
                break;
            }
            offset += entry.next_entry_offset as usize;
        }
        processes
    }

    fn image_name(name: UnicodeString) -> String {
        if name.buffer.is_null() || name.length == 0 {
            return String::new();
        }
        // SAFETY: the kernel points the name into the same buffer, valid for
        // `length` bytes, and keeps it 2-byte aligned.
        let units =
            unsafe { std::slice::from_raw_parts(name.buffer, usize::from(name.length) / 2) };
        String::from_utf16_lossy(units)
    }

    /// A process's command line, if it can be opened: processes of the same
    /// user can. Only the limited query right is asked for.
    pub(super) fn command_line(pid: u32) -> Option<String> {
        // SAFETY: plain call; the handle is closed below.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let mut buffer = vec![0u64; 512];
        let mut line = None;
        for _ in 0..2 {
            let bytes = u32::try_from(buffer.len() * 8).unwrap_or(u32::MAX);
            let mut needed = 0u32;
            // SAFETY: the buffer is writable for `bytes` bytes, `needed` is
            // a valid `u32`, and the handle is open.
            let status = unsafe {
                NtQueryInformationProcess(
                    handle,
                    ProcessCommandLineInformation,
                    buffer.as_mut_ptr().cast(),
                    bytes,
                    &mut needed,
                )
            };
            if matches!(
                status.0,
                STATUS_INFO_LENGTH_MISMATCH | STATUS_BUFFER_TOO_SMALL | STATUS_BUFFER_OVERFLOW
            ) {
                buffer = vec![0u64; (needed as usize).div_ceil(8) + 1];
                continue;
            }
            if status.is_ok() && buffer.len() * 8 >= size_of::<UnicodeString>() {
                // SAFETY: the call wrote a `UNICODE_STRING` at the start of
                // the buffer, pointing at the text later in the same buffer.
                let text = unsafe { buffer.as_ptr().cast::<UnicodeString>().read_unaligned() };
                line = Some(image_name(text));
            }
            break;
        }
        // SAFETY: the handle came from `OpenProcess` and is closed once.
        let _ = unsafe { CloseHandle(handle) };
        line
    }

    pub(super) fn logical_cpus() -> u32 {
        // SAFETY: plain call with no pointers.
        unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }
    }

    /// The PDH query with the three GPU counters, kept open between samples
    /// because utilisation is a rate of two collections.
    pub(super) struct GpuCounters {
        query: PDH_HQUERY,
        engine: PDH_HCOUNTER,
        dedicated: PDH_HCOUNTER,
        shared: PDH_HCOUNTER,
    }

    // SAFETY: PDH handles are not tied to the thread that opened them, and
    // `&mut self` on every use keeps two threads from using them at once.
    unsafe impl Send for GpuCounters {}

    impl GpuCounters {
        /// `None` when PDH or the GPU counter sets are not there.
        pub(super) fn open() -> Option<Self> {
            let mut query = PDH_HQUERY(std::ptr::null_mut());
            // SAFETY: `query` is a valid place for the handle.
            if unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) } != 0 {
                return None;
            }
            let mut counters = Self {
                query,
                engine: PDH_HCOUNTER(std::ptr::null_mut()),
                dedicated: PDH_HCOUNTER(std::ptr::null_mut()),
                shared: PDH_HCOUNTER(std::ptr::null_mut()),
            };
            let paths = [
                w!("\\GPU Engine(*)\\Utilization Percentage"),
                w!("\\GPU Process Memory(*)\\Dedicated Usage"),
                w!("\\GPU Process Memory(*)\\Shared Usage"),
            ];
            let slots = [
                &mut counters.engine,
                &mut counters.dedicated,
                &mut counters.shared,
            ];
            for (path, slot) in paths.into_iter().zip(slots) {
                // SAFETY: the query is open, the path is a static wide
                // string and `slot` a valid place for the handle. On failure
                // `counters` is dropped, which closes the query.
                if unsafe { PdhAddEnglishCounterW(query, path, 0, slot) } != 0 {
                    return None;
                }
            }
            // The first collection is the baseline for the utilisation rate.
            // SAFETY: the query is open.
            unsafe { PdhCollectQueryData(query) };
            Some(counters)
        }

        pub(super) fn read(&mut self, pid: u32) -> Option<GpuUsage> {
            // SAFETY: the query is open.
            if unsafe { PdhCollectQueryData(self.query) } != 0 {
                return None;
            }
            let engines = counter_array(self.engine);
            let dedicated = counter_array(self.dedicated)?;
            let shared = counter_array(self.shared)?;
            Some(gpu_for_process(
                pid,
                engines.as_deref(),
                &dedicated,
                &shared,
            ))
        }
    }

    impl Drop for GpuCounters {
        fn drop(&mut self) {
            // SAFETY: the query was opened by `open` and is closed once,
            // which also frees its counters.
            unsafe { PdhCloseQuery(self.query) };
        }
    }

    /// Instance names and values of a wildcard counter. Instances whose
    /// value is not valid yet are left out; `None` when the array cannot be
    /// read at all, as for a rate counter after one collection.
    fn counter_array(counter: PDH_HCOUNTER) -> Option<Vec<(String, f64)>> {
        let item_size = size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        let mut bytes = 0u32;
        // The instance list can grow between the size query and the read.
        for _ in 0..3 {
            let mut count = 0u32;
            let mut items =
                vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); (bytes as usize).div_ceil(item_size)];
            let buffer = (!items.is_empty()).then_some(items.as_mut_ptr());
            // SAFETY: `items` holds at least `bytes` bytes, suitably aligned
            // for the items PDH writes at its start; the names it points to
            // are written into the same buffer, which outlives their use.
            let status = unsafe {
                PdhGetFormattedCounterArrayW(
                    counter,
                    PDH_FMT_DOUBLE,
                    &mut bytes,
                    &mut count,
                    buffer,
                )
            };
            if status == PDH_MORE_DATA {
                continue;
            }
            if status != 0 {
                return None;
            }
            let count = (count as usize).min(items.len());
            let values = items[..count]
                .iter()
                .filter(|item| {
                    matches!(
                        item.FmtValue.CStatus,
                        PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA
                    )
                })
                .map(|item| {
                    // SAFETY: PDH_FMT_DOUBLE fills the double, and the name
                    // is a NUL-terminated string inside `items`.
                    let value = unsafe { item.FmtValue.Anonymous.doubleValue };
                    let name = unsafe { item.szName.to_string() }.unwrap_or_default();
                    (name, value)
                })
                .collect();
            return Some(values);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn process(pid: u32, parent_pid: u32, name: &str, create_time: i64) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent_pid,
            name: name.to_string(),
            create_time,
            cpu_time: 0,
            working_set: u64::from(pid) * 1000,
            private_working_set: 0,
        }
    }

    #[test]
    fn vmmem_wsl_is_the_vm_whatever_else_runs() {
        let processes = [
            process(4, 0, "System", 0),
            process(900, 4, "vmmem", 10),
            process(901, 4, "vmmemWSL", 20),
        ];
        assert_eq!(
            pick_vm_process(&processes, false),
            VmPick::Found(&processes[2])
        );
    }

    #[test]
    fn a_lone_legacy_vmmem_needs_a_sign_of_wsl() {
        let processes = [process(4, 0, "System", 0), process(900, 4, "vmmem", 10)];
        assert_eq!(
            pick_vm_process(&processes, true),
            VmPick::Found(&processes[1])
        );
        assert_eq!(
            pick_vm_process(&processes, false),
            VmPick::Ambiguous { candidates: 1 }
        );
    }

    #[test]
    fn several_vms_without_a_wsl_name_are_ambiguous() {
        let processes = [process(900, 4, "vmmem", 10), process(901, 4, "VMMEM", 20)];
        assert_eq!(
            pick_vm_process(&processes, true),
            VmPick::Ambiguous { candidates: 2 }
        );
        let two_users = [
            process(900, 4, "vmmemWSL", 10),
            process(901, 4, "vmmemWSL", 20),
        ];
        assert_eq!(
            pick_vm_process(&two_users, true),
            VmPick::Ambiguous { candidates: 2 }
        );
    }

    #[test]
    fn no_vm_process_means_not_running() {
        let processes = [
            process(4, 0, "System", 0),
            process(50, 4, "vmcompute.exe", 1),
        ];
        assert_eq!(pick_vm_process(&processes, true), VmPick::NotRunning);
        assert_eq!(pick_vm_process(&[], false), VmPick::NotRunning);
    }

    fn cpu(pid: u32, create_time: i64, cpu_time: u64, taken: Instant) -> CpuSample {
        CpuSample {
            pid,
            create_time,
            cpu_time,
            taken,
        }
    }

    #[test]
    fn cpu_percent_is_of_all_logical_processors() {
        let start = Instant::now();
        let later = start + Duration::from_secs(2);
        // Four seconds of CPU (in 100 ns units) over two seconds of eight CPUs.
        let percent = cpu_percent(&cpu(9, 1, 0, start), &cpu(9, 1, 40_000_000, later), 8).unwrap();
        assert!((percent - 25.0).abs() < 1e-9);
        // Never above the whole machine, whatever the rounding.
        let percent = cpu_percent(&cpu(9, 1, 0, start), &cpu(9, 1, u64::MAX, later), 8).unwrap();
        assert_eq!(percent, 100.0);
    }

    #[test]
    fn cpu_percent_needs_the_same_process_and_time_passing() {
        let start = Instant::now();
        let later = start + Duration::from_secs(1);
        let before = cpu(9, 1, 100, start);
        // The VM restarted: a new process, maybe even with the same PID.
        assert_eq!(cpu_percent(&before, &cpu(10, 1, 200, later), 4), None);
        assert_eq!(cpu_percent(&before, &cpu(9, 2, 200, later), 4), None);
        assert_eq!(cpu_percent(&before, &cpu(9, 1, 200, start), 4), None);
        assert_eq!(cpu_percent(&before, &cpu(9, 1, 50, later), 4), None);
        assert_eq!(cpu_percent(&before, &cpu(9, 1, 200, later), 0), None);
        assert_eq!(
            cpu_percent(&cpu(9, 1, 100, later), &cpu(9, 1, 200, start), 4),
            None
        );
    }

    #[test]
    fn docklet_processes_are_its_programs_their_children_and_its_wsl_clients() {
        let processes = [
            process(1, 0, "explorer.exe", 1),
            process(10, 1, "docklet-desktop.exe", 10),
            process(11, 10, "msedgewebview2.exe", 11),
            process(12, 11, "msedgewebview2.exe", 12),
            process(20, 1, "DOCKLET.EXE", 20),
            process(21, 20, "wsl.exe", 21),
            process(22, 21, "conhost.exe", 22),
            // The keepalive: its `docklet start` has exited.
            process(30, 99, "wsl.exe", 30),
            // The user's own shell in another distro.
            process(31, 1, "wsl.exe", 31),
            // An old process naming a parent PID that Docklet reused.
            process(40, 20, "notepad.exe", 5),
            process(2, 0, "svchost.exe", 2),
        ];
        let members = docklet_processes(&processes, |process| process.pid == 30);
        let pids: Vec<_> = members.iter().map(|p| p.pid).collect();
        assert_eq!(pids, [10, 11, 12, 20, 21, 22, 30]);

        let totals = docklet_totals(&members);
        assert_eq!(totals.count, 7);
        assert_eq!(totals.wsl_clients, 2);
        assert_eq!(
            totals.working_set_bytes,
            (10 + 11 + 12 + 20 + 21 + 22 + 30) * 1000
        );
    }

    #[test]
    fn only_unattached_wsl_processes_have_their_command_line_read() {
        let processes = [
            process(20, 1, "docklet.exe", 20),
            process(21, 20, "wsl.exe", 21),
            process(30, 99, "wsl.exe", 30),
            process(31, 1, "bash.exe", 31),
        ];
        let mut asked = Vec::new();
        docklet_processes(&processes, |process| {
            asked.push(process.pid);
            false
        });
        assert_eq!(asked, [30]);
    }

    #[test]
    fn nothing_of_docklet_runs() {
        let processes = [process(1, 0, "explorer.exe", 1)];
        let members = docklet_processes(&processes, |_| false);
        assert_eq!(docklet_totals(&members), DockletProcesses::default());
    }

    #[test]
    fn image_names_compare_like_windows_does() {
        let named = |name: &str| process(1, 0, name, 0);
        assert!(named("WSL.EXE").is_named("wsl.exe"));
        assert!(named("wsl.exe").is_named("wsl"));
        assert!(named("vmmemWSL").is_named("VMMEMWSL"));
        assert!(!named("vmmemWSL").is_named("vmmem"));
        assert!(!named("wsl.exe.bak").is_named("wsl.exe"));
        assert!(!named("exe").is_named("wsl.exe"));
        assert!(named("Ünï.exe").is_named("Ünï"));
    }

    #[test]
    fn wsl_command_lines_for_the_docklet_distro() {
        assert!(names_docklet_distro(
            r#""C:\WINDOWS\system32\wsl.exe" -d docklet -u root --exec /bin/sleep infinity"#
        ));
        assert!(names_docklet_distro(
            "wsl.exe -d docklet -u root --exec /usr/local/bin/docklet-relay /var/run/docker.sock"
        ));
        assert!(names_docklet_distro("wsl --distribution Docklet"));
        assert!(!names_docklet_distro("wsl.exe -d Ubuntu"));
        assert!(!names_docklet_distro("wsl.exe -d docklet-old"));
        assert!(!names_docklet_distro("wsl.exe --exec echo docklet"));
        assert!(!names_docklet_distro("wsl.exe -d"));
        assert!(!names_docklet_distro(""));
    }

    #[test]
    fn gpu_instances_name_their_process() {
        assert_eq!(
            gpu_instance_pid(
                "pid_1234_luid_0x00000000_0x0000D1F2_phys_0_eng_3_engtype_VideoDecode"
            ),
            Some(1234)
        );
        assert_eq!(
            gpu_instance_pid("pid_77_luid_0x00000000_0x0000D1F2_phys_0"),
            Some(77)
        );
        assert_eq!(gpu_instance_pid("_Total"), None);
        assert_eq!(gpu_instance_pid("pid__luid"), None);
    }

    fn items(list: &[(&str, f64)]) -> Vec<(String, f64)> {
        list.iter()
            .map(|(name, value)| (name.to_string(), *value))
            .collect()
    }

    #[test]
    fn gpu_use_is_the_busiest_engine_and_the_summed_memory() {
        let engines = items(&[
            ("pid_900_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 30.0),
            ("pid_900_luid_0x0_0x1_phys_0_eng_1_engtype_Copy", 5.0),
            ("pid_900_luid_0x0_0x1_phys_0_eng_3_engtype_Compute_0", 62.5),
            ("pid_9000_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 99.0),
            ("pid_12_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 80.0),
        ]);
        let dedicated = items(&[
            ("pid_900_luid_0x0_0x1_phys_0", 1_000_000.0),
            ("pid_900_luid_0x0_0x2_phys_0", 24_000.0),
            ("pid_12_luid_0x0_0x1_phys_0", 7.0),
        ]);
        let shared = items(&[("pid_900_luid_0x0_0x1_phys_0", 4096.0)]);
        let usage = gpu_for_process(900, Some(&engines), &dedicated, &shared);
        assert_eq!(
            usage,
            GpuUsage {
                utilization_percent: Some(62.5),
                dedicated_bytes: 1_024_000,
                shared_bytes: 4096,
            }
        );
    }

    #[test]
    fn gpu_use_of_an_idle_or_unmeasured_process() {
        let usage = gpu_for_process(5, Some(&[]), &[], &[]);
        assert_eq!(usage.utilization_percent, Some(0.0));
        assert_eq!(usage.dedicated_bytes, 0);
        // Utilisation is a rate: nothing to show after one collection.
        assert_eq!(gpu_for_process(5, None, &[], &[]).utilization_percent, None);
        // Out-of-range values are clamped or ignored.
        let engines = items(&[
            ("pid_5_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 140.0),
            ("pid_5_luid_0x0_0x1_phys_0_eng_1_engtype_3D", f64::NAN),
        ]);
        let usage = gpu_for_process(5, Some(&engines), &[], &[]);
        assert_eq!(usage.utilization_percent, Some(100.0));
    }

    #[test]
    fn the_vhdx_lives_in_the_distro_directory() {
        let path = vhdx_path(Path::new("C:\\Users\\me\\AppData\\Local\\Docklet"));
        assert!(path.ends_with("distro/ext4.vhdx") || path.ends_with("distro\\ext4.vhdx"));
    }

    #[cfg(not(windows))]
    #[test]
    fn sampling_is_unsupported_off_windows() {
        let mut sampler = UsageSampler::new(PathBuf::from("/tmp/docklet"));
        let err = sampler.sample().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    /// Reads the real process list, PDH counters and disk. Read-only, starts
    /// nothing, and is bounded by its two samples and one pause.
    #[cfg(windows)]
    #[test]
    #[ignore = "runner only"]
    fn a_real_sample_has_sane_figures() {
        let data_dir = std::env::temp_dir().join("docklet-usage-test");
        let mut sampler = UsageSampler::new(data_dir);
        let first = sampler.sample().expect("a first sample");
        std::thread::sleep(Duration::from_millis(500));
        let second = sampler.sample().expect("a second sample");
        for usage in [&first, &second] {
            if let VmStatus::Found(vm) = &usage.vm {
                assert!(vm.working_set_bytes > 0);
                if let Some(cpu) = vm.cpu_percent {
                    assert!((0.0..=100.0).contains(&cpu));
                }
            }
            if let Some(gpu) = usage.gpu
                && let Some(percent) = gpu.utilization_percent
            {
                assert!((0.0..=100.0).contains(&percent));
            }
            // The temporary data directory holds no distro.
            assert_eq!(usage.disk.vhdx_bytes, None);
            assert!(usage.disk.free_bytes.is_some_and(|free| free > 0));
        }
        // The process list is never empty, and this test is not Docklet.
        assert!(windows_impl::process_list().unwrap().len() > 10);
    }
}
