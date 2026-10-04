//! Package power and CPU temperature, through `PawnIO`.
//!
//! Both live in model-specific registers and, on AMD, in a system-management
//! network register, and user mode cannot read either. Instead of a driver of our
//! own, this module uses [`PawnIO`](https://pawnio.eu) when it is installed: a
//! signed kernel driver that runs signed "modules" (small sandboxed programs) and
//! exposes them to user mode through `PawnIOLib.dll`. Its C API, from
//! `PawnIOLib.h`: `pawnio_version`, `pawnio_open` (an executor handle),
//! `pawnio_load` (a module blob into that executor), `pawnio_execute` (call one of
//! the module's exported functions with `u64` in and out arrays) and
//! `pawnio_close`. The modules come from `namazso/PawnIO.Modules` (LGPL-2.1, see
//! `crates/ot-probe/pawnio/README.md`) and are embedded with `include_bytes!`;
//! the driver checks their signature. Each exports `ioctl_read_msr` (`in[0]` the
//! MSR index, `out[0]` its value) behind an allow-list of registers; the AMD one
//! also exports `ioctl_read_smn` (`in[0]` the SMN offset, `out[0]` its value),
//! implemented as an index/data pair in the PCI configuration space of the data
//! fabric at bus 0, device 0, function 0.
//!
//! `PawnIOLib.dll` is never linked: the app must run without `PawnIO`. It is loaded
//! by full path from the install directory (the `InstallDir` value under
//! `HKLM\SOFTWARE\PawnIO` or the `InstallLocation` of its Uninstall entry, then
//! `%ProgramFiles%\PawnIO`), never by bare name, so a DLL of the same name on the
//! search path cannot stand in for it. Without it the feature is absent and one
//! line at info level says where to get it.
//!
//! # Registers
//!
//! Intel (SDM Vol. 3B §15.10 "RAPL interfaces" and Vol. 4 Table 2-2):
//! - `MSR_RAPL_POWER_UNIT` (0x606) bits 12:8 are the energy status unit, `ESU`;
//!   one count of an energy counter is `1 / 2^ESU` joules (2^-16 J on most
//!   parts, 2^-14 J on Atom).
//! - `MSR_PKG_ENERGY_STATUS` (0x611) bits 31:0 count the package's energy since
//!   reset, wrapping at 2^32; at 2^-16 J that is 65 kJ, or eleven minutes at 100 W,
//!   so one wrap at most can fall inside a sampling interval and the wrapping
//!   difference of two reads is the energy spent between them.
//! - `MSR_TEMPERATURE_TARGET` (0x1A2) bits 23:16 are the temperature at which the
//!   thermal control circuit engages, `TjMax`, which is what the digital sensors
//!   count down from.
//! - `IA32_PACKAGE_THERM_STATUS` (0x1B1) bits 22:16 are the package's digital
//!   readout in degrees below `TjMax` (the hottest sensor on the die), so the
//!   temperature is `TjMax - readout`. Not every part has it; `IA32_THERM_STATUS`
//!   (0x19C) carries the same field for the core the reading thread runs on, with
//!   bit 31 saying whether the readout is valid, and is the fallback.
//!
//! AMD Zen (PPR for Family 17h Model 01h, document 54945, and later families'
//! PPRs, which keep these layouts; `LibreHardwareMonitor`'s `Amd17Cpu` reads the
//! same registers):
//! - `MSR C001_0299` (`RAPL_PWR_UNIT`) bits 12:8 are the energy unit, as Intel's.
//! - `MSR C001_029B` (`PKG_ENERGY_STAT`) bits 31:0 are the package energy counter.
//! - SMN `0x0005_9800`, `SMU::THM::THM_TCON_CUR_TMP`: bits 31:21 are `CUR_TEMP` in
//!   eighths of a degree; bit 19, `CUR_TEMP_RANGE_SEL`, says the value is on the
//!   -49..206 °C range rather than 0..255, so 49 is subtracted when it is set.
//!   This is Tctl, the control temperature the fan curve follows. Some models
//!   report Tctl a fixed offset above the die temperature (Tdie: 20 °C on the
//!   first Threadripper and some Ryzen 7 1x00X parts, 27 °C on the 2990WX); the
//!   offset table is not applied here, and the UI labels the reading "Tctl" so
//!   it does not claim to be a die temperature.
//!
//! # Where the reads run
//!
//! `pawnio_execute` runs the module on whichever processor the calling thread is
//! on; nothing is pinned. The registers read here are package-scoped: every core
//! of a package returns the same energy counter and package thermal status, and
//! the sampler thread stays inside one package on a single-socket machine, which
//! is every desktop and laptop. On a multi-socket machine the energy reading is
//! that of whichever socket the thread happens to be on, and two consecutive
//! reads on different sockets would difference two unrelated counters, so the
//! result is dropped when it is implausible (above [`MAX_WATTS`]). The SMN read
//! goes through socket 0's data fabric and is socket 0's temperature. The Intel
//! fallback (`IA32_THERM_STATUS`) is per core and reads the current core.
//!
//! The AMD SMN index/data pair is shared with every other tool reading the data
//! fabric, so those tools hand it off through the named mutex
//! `Global\Access_PCI` (the module's own `@warning` asks for it; `WinRing0`-era
//! convention). It is held for the two configuration accesses and the sample
//! skips the temperature when another holder keeps it past a few milliseconds.
//!
//! # Cost
//!
//! One `sample` is two or three `pawnio_execute` calls (energy, package thermal
//! status; the AMD path adds the SMN read): on this machine (i7-10710U, Intel,
//! `PawnIO` 2.2.0, `pawnio_cost` in the tests, dev profile) 0.026 ms mean and
//! 1.2 ms worst over 100 calls. Nothing to budget for; it runs every pass.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Instant;

use ot_model::cpu::ThermalSensor;
use windows::core::{s, w, HRESULT, PCSTR, PCWSTR};
use windows::Win32::Foundation::{FreeLibrary, HANDLE, HMODULE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

use super::tags::OwnedHandle;

/// Intel RAPL and thermal MSRs.
const MSR_RAPL_POWER_UNIT: u32 = 0x606;
const MSR_PKG_ENERGY_STATUS: u32 = 0x611;
const MSR_TEMPERATURE_TARGET: u32 = 0x1A2;
const IA32_PACKAGE_THERM_STATUS: u32 = 0x1B1;
const IA32_THERM_STATUS: u32 = 0x19C;
/// AMD Zen RAPL MSRs and the thermal SMN register.
const AMD_RAPL_PWR_UNIT: u32 = 0xC001_0299;
const AMD_PKG_ENERGY_STAT: u32 = 0xC001_029B;
const AMD_THM_TCON_CUR_TMP: u32 = 0x0005_9800;

/// A rate above this is not a package reading (a counter reset after sleep, or
/// two sockets differenced) and is dropped.
const MAX_WATTS: f32 = 5000.0;
/// How long to wait for the shared PCI configuration lock before skipping the
/// AMD temperature this pass.
const PCI_LOCK_WAIT_MS: u32 = 5;

/// The signed module blobs, from `crates/ot-probe/pawnio/`.
const INTEL_MSR_MODULE: &[u8] = include_bytes!("../../../pawnio/IntelMSR.bin");
const AMD_FAMILY17_MODULE: &[u8] = include_bytes!("../../../pawnio/AMDFamily17.bin");

type VersionFn = unsafe extern "system" fn(*mut u32) -> HRESULT;
type OpenFn = unsafe extern "system" fn(*mut HANDLE) -> HRESULT;
type LoadFn = unsafe extern "system" fn(HANDLE, *const u8, usize) -> HRESULT;
type ExecuteFn = unsafe extern "system" fn(
    HANDLE,
    PCSTR,
    *const u64,
    usize,
    *mut u64,
    usize,
    *mut usize,
) -> HRESULT;
type CloseFn = unsafe extern "system" fn(HANDLE) -> HRESULT;

/// Which module is loaded, and so which registers to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Only x86 has CPUID to tell the vendor; elsewhere `vendor()` answers `None` and
// this is reached from its tests alone, which run on every target.
#[cfg_attr(
    not(any(target_arch = "x86", target_arch = "x86_64")),
    allow(dead_code)
)]
enum Vendor {
    Intel,
    AmdZen,
}

/// One pass's readings. Either can be absent: the first pass has no rate, and a
/// machine may have power without a thermal sensor `PawnIO` can reach.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct Reading {
    pub package_watts: Option<f32>,
    pub package_celsius: Option<f32>,
}

/// `PawnIOLib.dll`, loaded by path, with the exports resolved.
#[derive(Debug)]
struct Lib {
    module: HMODULE,
    version: VersionFn,
    open: OpenFn,
    load: LoadFn,
    execute: ExecuteFn,
    close: CloseFn,
}

impl Drop for Lib {
    fn drop(&mut self) {
        // SAFETY: the module came from LoadLibraryW and is freed once, after every
        // handle it hands out is closed (`PawnIo::drop` runs first).
        unsafe {
            let _ = FreeLibrary(self.module);
        }
    }
}

/// An executor with the module for this CPU loaded, and the state the rates and
/// fallbacks need between passes.
#[derive(Debug)]
pub(super) struct PawnIo {
    handle: HANDLE,
    vendor: Vendor,
    /// Joules per count of the energy counter.
    energy_unit: f64,
    /// The energy counter's low 32 bits at the last read, and when.
    energy: Option<(u32, Instant)>,
    /// Intel: `TjMax`, read once.
    tjmax: Option<f32>,
    /// Intel: the package thermal status MSR was refused, so the core's is read.
    package_therm_refused: bool,
    /// AMD: the lock every tool takes around the data fabric's index/data pair.
    pci_lock: Option<OwnedHandle>,
    /// Whether the first temperature read worked; the capability flag.
    thermals: bool,
    /// Declared last so it is dropped after the executor is closed.
    lib: Lib,
}

// SAFETY: the executor handle and the module handle are process-wide tokens,
// valid from any thread; nothing here is thread-affine. The sampler owns the
// probe and uses it from its own thread only.
unsafe impl Send for PawnIo {}

/// Says once per process where `PawnIO` comes from.
static NOTED_MISSING: Once = Once::new();

impl PawnIo {
    /// Load the library and the module for this CPU, and prime the readings.
    /// `None` when `PawnIO` is not installed, this CPU has no module, or the first
    /// read fails; the reason is logged.
    pub fn open() -> Option<Self> {
        let Some(vendor) = vendor() else {
            tracing::debug!("no PawnIO module for this CPU; no package power or temperature");
            return None;
        };
        let Some(lib) = Lib::load() else {
            NOTED_MISSING.call_once(|| {
                tracing::info!(
                    "PawnIO is not installed; install PawnIO from https://pawnio.eu to see package power and temperature"
                );
            });
            return None;
        };
        let mut version = 0u32;
        // SAFETY: `version` is a valid out-pointer; the signature is the header's.
        let hr = unsafe { (lib.version)(&raw mut version) };
        if hr.is_ok() {
            tracing::debug!(
                "PawnIOLib {}.{}.{}",
                version >> 16,
                (version >> 8) & 0xFF,
                version & 0xFF
            );
        }

        let mut handle = HANDLE::default();
        // SAFETY: `handle` is a valid out-pointer.
        let hr = unsafe { (lib.open)(&raw mut handle) };
        if hr.is_err() {
            tracing::warn!(%hr, "PawnIO is installed but its executor could not be opened");
            return None;
        }
        let (blob, name) = match vendor {
            Vendor::Intel => (INTEL_MSR_MODULE, "IntelMSR"),
            Vendor::AmdZen => (AMD_FAMILY17_MODULE, "AMDFamily17"),
        };
        // SAFETY: the blob is a static byte slice passed with its length; the
        // handle is open.
        let hr = unsafe { (lib.load)(handle, blob.as_ptr(), blob.len()) };
        if hr.is_err() {
            tracing::warn!(%hr, "PawnIO refused the {name} module");
            // SAFETY: the handle is open and closed exactly once.
            let _ = unsafe { (lib.close)(handle) };
            return None;
        }

        let mut this = Self {
            handle,
            vendor,
            energy_unit: 0.0,
            energy: None,
            tjmax: None,
            package_therm_refused: false,
            pci_lock: None,
            thermals: false,
            lib,
        };
        let unit_msr = match vendor {
            Vendor::Intel => MSR_RAPL_POWER_UNIT,
            Vendor::AmdZen => AMD_RAPL_PWR_UNIT,
        };
        let Some(unit) = this.read_msr(unit_msr).map(energy_unit) else {
            tracing::warn!("the {name} module loaded but the energy unit could not be read");
            return None;
        };
        this.energy_unit = unit;
        if vendor == Vendor::AmdZen {
            // SAFETY: a named mutex, created or opened; the handle is owned by the
            // guard. `None` attributes means the default security descriptor.
            this.pci_lock = unsafe { CreateMutexW(None, false, w!("Global\\Access_PCI")) }
                .ok()
                .map(OwnedHandle);
            if this.pci_lock.is_none() {
                tracing::debug!(
                    "the Access_PCI lock could not be opened; SMN reads run without it"
                );
            }
        }
        // The first pass: primes the energy counter and tells whether the
        // temperature can be read at all.
        let first = this.sample();
        if this.energy.is_none() {
            tracing::warn!("the {name} module loaded but the energy counter could not be read");
            return None;
        }
        this.thermals = first.package_celsius.is_some();
        tracing::debug!(
            "PawnIO {name} module loaded: energy unit {unit:e} J, thermals {}",
            this.thermals
        );
        Some(this)
    }

    /// Which sensor the temperature comes from.
    pub fn sensor(&self) -> ThermalSensor {
        match self.vendor {
            Vendor::Intel => ThermalSensor::Package,
            Vendor::AmdZen => ThermalSensor::Tctl,
        }
    }

    /// Whether the temperature read on the first pass.
    pub fn thermals(&self) -> bool {
        self.thermals
    }

    /// One pass: the package's power since the last call, and its temperature.
    pub fn sample(&mut self) -> Reading {
        let now = Instant::now();
        let mut reading = Reading::default();
        let energy_msr = match self.vendor {
            Vendor::Intel => MSR_PKG_ENERGY_STATUS,
            Vendor::AmdZen => AMD_PKG_ENERGY_STAT,
        };
        if let Some(raw) = self.read_msr(energy_msr) {
            let raw = raw as u32;
            if let Some((prev, at)) = self.energy {
                let secs = now.duration_since(at).as_secs_f64();
                reading.package_watts = watts_between(prev, raw, self.energy_unit, secs);
            }
            self.energy = Some((raw, now));
        }
        reading.package_celsius = match self.vendor {
            Vendor::Intel => self.intel_celsius(),
            Vendor::AmdZen => self.amd_tctl(),
        };
        reading
    }

    /// `TjMax - readout` from the package sensor, or from the current core's when
    /// the package MSR is refused.
    fn intel_celsius(&mut self) -> Option<f32> {
        if self.tjmax.is_none() {
            self.tjmax = self.read_msr(MSR_TEMPERATURE_TARGET).and_then(intel_tjmax);
        }
        let tjmax = self.tjmax?;
        if !self.package_therm_refused {
            if let Some(status) = self.read_msr(IA32_PACKAGE_THERM_STATUS) {
                return Some(intel_package_celsius(tjmax, status));
            }
            tracing::debug!("IA32_PACKAGE_THERM_STATUS refused; reading the core's sensor");
            self.package_therm_refused = true;
        }
        self.read_msr(IA32_THERM_STATUS)
            .and_then(|status| intel_core_celsius(tjmax, status))
    }

    /// Tctl from the thermal controller, under the shared PCI lock.
    fn amd_tctl(&self) -> Option<f32> {
        let held = self.pci_lock.as_ref().and_then(|lock| {
            // SAFETY: the mutex handle is open; a bounded wait.
            let waited = unsafe { WaitForSingleObject(lock.0, PCI_LOCK_WAIT_MS) };
            (waited == WAIT_OBJECT_0 || waited == WAIT_ABANDONED).then_some(lock)
        });
        if self.pci_lock.is_some() && held.is_none() {
            // Another tool is in the middle of an index/data pair; skip this pass.
            return None;
        }
        let raw = self.execute(s!("ioctl_read_smn"), u64::from(AMD_THM_TCON_CUR_TMP));
        if let Some(lock) = held {
            // SAFETY: this thread acquired the mutex just above.
            let _ = unsafe { ReleaseMutex(lock.0) };
        }
        raw.map(amd_tctl)
    }

    fn read_msr(&self, index: u32) -> Option<u64> {
        self.execute(s!("ioctl_read_msr"), u64::from(index))
    }

    /// Call one of the module's exports with one input and one output word.
    fn execute(&self, name: PCSTR, input: u64) -> Option<u64> {
        let mut out = 0u64;
        let mut returned = 0usize;
        // SAFETY: `name` is a NUL-terminated static string; the input and output
        // are single words passed with count 1 and outlive the call; the handle
        // is open for the life of `self`.
        let hr = unsafe {
            (self.lib.execute)(
                self.handle,
                name,
                &raw const input,
                1,
                &raw mut out,
                1,
                &raw mut returned,
            )
        };
        if hr.is_err() {
            tracing::trace!(%hr, input, "PawnIO call failed");
            return None;
        }
        Some(out)
    }
}

impl Drop for PawnIo {
    fn drop(&mut self) {
        // SAFETY: the executor handle came from pawnio_open and is closed exactly
        // once, before the library is freed by `Lib::drop`.
        let _ = unsafe { (self.lib.close)(self.handle) };
    }
}

impl Lib {
    /// Load `PawnIOLib.dll` from the first install directory that has it.
    fn load() -> Option<Self> {
        let mut tried: Vec<PathBuf> = Vec::new();
        for dir in install_dirs() {
            if tried.contains(&dir) {
                continue;
            }
            let path = dir.join("PawnIOLib.dll");
            if let Some(lib) = Self::load_path(&path) {
                tracing::debug!("PawnIOLib loaded from {}", path.display());
                return Some(lib);
            }
            tried.push(dir);
        }
        None
    }

    fn load_path(path: &Path) -> Option<Self> {
        if !path.is_file() {
            return None;
        }
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: a NUL-terminated full path; the module is freed on drop.
        let module = unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }.ok()?;
        let module = Module(module);
        // SAFETY: the exports have the signatures in PawnIOLib.h; a missing one
        // makes the whole library unusable rather than one call.
        let lib = unsafe {
            Self {
                module: module.0,
                version: std::mem::transmute::<*const c_void, VersionFn>(export(
                    module.0,
                    s!("pawnio_version"),
                )?),
                open: std::mem::transmute::<*const c_void, OpenFn>(export(
                    module.0,
                    s!("pawnio_open"),
                )?),
                load: std::mem::transmute::<*const c_void, LoadFn>(export(
                    module.0,
                    s!("pawnio_load"),
                )?),
                execute: std::mem::transmute::<*const c_void, ExecuteFn>(export(
                    module.0,
                    s!("pawnio_execute"),
                )?),
                close: std::mem::transmute::<*const c_void, CloseFn>(export(
                    module.0,
                    s!("pawnio_close"),
                )?),
            }
        };
        // `lib` now owns the module.
        std::mem::forget(module);
        Some(lib)
    }
}

/// A module freed on drop, for the window between loading it and resolving its
/// exports.
struct Module(HMODULE);

impl Drop for Module {
    fn drop(&mut self) {
        // SAFETY: loaded by `Lib::load_path`, freed once.
        unsafe {
            let _ = FreeLibrary(self.0);
        }
    }
}

/// One export's address, as a data pointer for the caller to cast.
fn export(module: HMODULE, name: PCSTR) -> Option<*const c_void> {
    // SAFETY: the module is loaded and the name NUL-terminated.
    let f = unsafe { GetProcAddress(module, name) }?;
    Some(f as *const c_void)
}

/// Where `PawnIO` may be installed, most authoritative first: the installer's
/// registry hints, then the default location.
fn install_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::with_capacity(3);
    if let Some(dir) = reg_string(w!("SOFTWARE\\PawnIO"), w!("InstallDir")) {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(dir) = reg_string(
        w!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\PawnIO"),
        w!("InstallLocation"),
    ) {
        dirs.push(PathBuf::from(dir));
    }
    let program_files = std::env::var_os("ProgramFiles")
        .map_or_else(|| PathBuf::from(r"C:\Program Files"), PathBuf::from);
    dirs.push(program_files.join("PawnIO"));
    dirs
}

/// A string value under `HKLM`, or `None` when the key, the value or the type is
/// not there.
fn reg_string(subkey: PCWSTR, value: PCWSTR) -> Option<String> {
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: the buffer and its byte size agree; the names are NUL-terminated.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey,
            value,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&raw mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let units = (size as usize / 2).min(buf.len());
    let end = buf[..units].iter().position(|&c| c == 0).unwrap_or(units);
    let s = String::from_utf16_lossy(&buf[..end]).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

/// The CPU this is, from CPUID: leaf 0's vendor string and leaf 1's family.
/// `None` when there is no module for it.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn vendor() -> Option<Vendor> {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::__cpuid;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::__cpuid;
    // Leaves 0 and 1 are defined on every x86 processor Windows runs on.
    let leaf0 = __cpuid(0);
    let mut name = [0u8; 12];
    name[..4].copy_from_slice(&leaf0.ebx.to_le_bytes());
    name[4..8].copy_from_slice(&leaf0.edx.to_le_bytes());
    name[8..].copy_from_slice(&leaf0.ecx.to_le_bytes());
    let family = cpuid_family(__cpuid(1).eax);
    classify(&name, family)
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn vendor() -> Option<Vendor> {
    None
}

/// The display family from CPUID leaf 1 EAX: the base family in bits 11:8, plus
/// the extended family in bits 27:20 when the base is 0xF (which every AMD Zen is:
/// 0xF + 0x8 = 0x17).
// Only x86 has CPUID to tell the vendor; elsewhere `vendor()` answers `None` and
// this is reached from its tests alone, which run on every target.
#[cfg_attr(
    not(any(target_arch = "x86", target_arch = "x86_64")),
    allow(dead_code)
)]
fn cpuid_family(eax: u32) -> u32 {
    let base = (eax >> 8) & 0xF;
    if base == 0xF {
        base + ((eax >> 20) & 0xFF)
    } else {
        base
    }
}

/// Which module fits a vendor string and family: Intel takes `IntelMSR`; AMD takes
/// `AMDFamily17` for the Zen families (17h, 19h, 1Ah) and nothing before them.
// Only x86 has CPUID to tell the vendor; elsewhere `vendor()` answers `None` and
// this is reached from its tests alone, which run on every target.
#[cfg_attr(
    not(any(target_arch = "x86", target_arch = "x86_64")),
    allow(dead_code)
)]
fn classify(name: &[u8; 12], family: u32) -> Option<Vendor> {
    match name {
        b"GenuineIntel" => Some(Vendor::Intel),
        b"AuthenticAMD" if matches!(family, 0x17 | 0x19 | 0x1A) => Some(Vendor::AmdZen),
        _ => None,
    }
}

/// Joules per count of an energy counter, from a RAPL power unit register: bits
/// 12:8 are the exponent of a negative power of two.
fn energy_unit(raw: u64) -> f64 {
    let esu = (raw >> 8) & 0x1F;
    1.0 / f64::from(1u32 << esu)
}

/// Watts from two reads of a 32-bit wrapping energy counter `secs` apart.
fn watts_between(prev: u32, now: u32, unit_joules: f64, secs: f64) -> Option<f32> {
    if secs <= 0.0 {
        return None;
    }
    let joules = f64::from(now.wrapping_sub(prev)) * unit_joules;
    let watts = (joules / secs) as f32;
    (watts <= MAX_WATTS).then_some(watts)
}

/// `TjMax` from `MSR_TEMPERATURE_TARGET`: bits 23:16, which some virtual machines
/// leave at zero.
fn intel_tjmax(raw: u64) -> Option<f32> {
    let target = (raw >> 16) & 0xFF;
    (target > 0).then_some(target as f32)
}

/// Degrees from `IA32_PACKAGE_THERM_STATUS`: bits 22:16 are degrees below `TjMax`.
fn intel_package_celsius(tjmax: f32, status: u64) -> f32 {
    tjmax - ((status >> 16) & 0x7F) as f32
}

/// Degrees from a core's `IA32_THERM_STATUS`, which has the same readout and a
/// valid bit (31).
fn intel_core_celsius(tjmax: f32, status: u64) -> Option<f32> {
    (status & (1 << 31) != 0).then(|| intel_package_celsius(tjmax, status))
}

/// Tctl from `THM_TCON_CUR_TMP`: bits 31:21 in eighths of a degree, less 49 when
/// bit 19 selects the shifted range.
fn amd_tctl(raw: u64) -> f32 {
    let eighths = (raw >> 21) & 0x7FF;
    let offset = if raw & (1 << 19) != 0 { 49.0 } else { 0.0 };
    eighths as f32 / 8.0 - offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_energy_unit_is_a_negative_power_of_two() {
        // 0x000A1003 is the usual Core value: ESU 16. Atom parts report ESU 14.
        assert_eq!(energy_unit(0x000A_1003), 1.0 / 65536.0);
        assert_eq!(energy_unit(0x000A_0E03), 1.0 / 16384.0);
        // Only bits 12:8 are the exponent.
        assert_eq!(energy_unit(0xFFFF_F0FF | (16 << 8)), 1.0 / 65536.0);
    }

    #[test]
    fn watts_follow_the_counter_across_a_wrap() {
        let unit = 1.0 / 65536.0;
        // 65536 counts is one joule; over half a second that is two watts.
        assert_eq!(watts_between(1000, 1000 + 65536, unit, 0.5), Some(2.0));
        // Wrapping past 2^32 counts the same joule.
        assert_eq!(
            watts_between(u32::MAX - 999, 65536 - 1000, unit, 0.5),
            Some(2.0)
        );
        assert_eq!(watts_between(5, 5, unit, 1.0), Some(0.0));
        assert_eq!(watts_between(5, 6, unit, 0.0), None, "no interval, no rate");
        // A reset counter differenced against the old value is not a reading.
        assert_eq!(watts_between(u32::MAX / 2, 0, unit, 0.001), None);
    }

    #[test]
    fn intel_degrees_count_down_from_tjmax() {
        assert_eq!(intel_tjmax(0x0064_0000), Some(100.0));
        assert_eq!(intel_tjmax(0), None);
        // Readout 39 below a TjMax of 100.
        assert_eq!(intel_package_celsius(100.0, 0x8827_0000), 61.0);
        assert_eq!(intel_core_celsius(100.0, 0x8827_0000), Some(61.0));
        assert_eq!(
            intel_core_celsius(100.0, 0x0027_0000),
            None,
            "valid bit clear"
        );
    }

    #[test]
    fn amd_tctl_is_in_eighths_with_the_range_offset() {
        // 61.0 °C: 488 eighths, range bit clear.
        assert_eq!(amd_tctl(488 << 21), 61.0);
        // The same bits with the -49 range selected.
        assert_eq!(amd_tctl((488 << 21) | (1 << 19)), 12.0);
        // Low bits are other fields and do not leak in.
        assert_eq!(amd_tctl((488 << 21) | 0x7_FFFF), 61.0);
    }

    #[test]
    fn the_vendor_string_and_family_pick_the_module() {
        assert_eq!(classify(b"GenuineIntel", 6), Some(Vendor::Intel));
        assert_eq!(classify(b"AuthenticAMD", 0x17), Some(Vendor::AmdZen));
        assert_eq!(classify(b"AuthenticAMD", 0x19), Some(Vendor::AmdZen));
        assert_eq!(classify(b"AuthenticAMD", 0x1A), Some(Vendor::AmdZen));
        assert_eq!(
            classify(b"AuthenticAMD", 0x15),
            None,
            "Bulldozer has no module"
        );
        assert_eq!(
            classify(b"AuthenticAMD", 0x18),
            None,
            "Hygon is not on the list"
        );
        assert_eq!(classify(b"HygonGenuine", 0x18), None);
        // Zen 2 (family 17h model 71h): base family F, extended 8.
        assert_eq!(cpuid_family(0x0087_0F10), 0x17);
        // Comet Lake: family 6, no extension.
        assert_eq!(cpuid_family(0x000A_0660), 6);
    }

    #[test]
    fn this_machine_reads_through_pawnio_when_it_is_installed() {
        let Some(mut probe) = PawnIo::open() else {
            eprintln!("PawnIO is not installed here, or this CPU has no module; nothing to read");
            return;
        };
        // `open` took the first pass; the temperature is known from it.
        assert!(probe.thermals(), "the first temperature read failed");
        std::thread::sleep(std::time::Duration::from_millis(250));
        let r = probe.sample();
        let watts = r.package_watts.expect("a rate on the second pass");
        assert!((0.1..=1000.0).contains(&watts), "{r:?}");
        let celsius = r.package_celsius.expect("a temperature");
        assert!((0.0..=120.0).contains(&celsius), "{r:?}");
        eprintln!("{:?}: {r:?}", probe.sensor());
    }

    /// What a sample costs here. Ignored because it measures the machine it runs
    /// on, and needs `PawnIO` installed:
    ///
    /// ```text
    /// cargo test -p ot-probe --release pawnio_cost -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "measures this machine; run by hand"]
    fn pawnio_cost() {
        let mut probe = PawnIo::open().expect("PawnIO with a module for this CPU");
        std::thread::sleep(std::time::Duration::from_millis(200));
        let n = 100;
        let mut worst = 0f64;
        let mut total = 0f64;
        let mut last = Reading::default();
        for _ in 0..n {
            let t = Instant::now();
            last = probe.sample();
            let ms = t.elapsed().as_secs_f64() * 1e3;
            worst = worst.max(ms);
            total += ms;
        }
        let mean = total / f64::from(n);
        println!(
            "pawnio sample  mean {mean:>7.3} ms   worst {worst:>7.3} ms  ({:?}, last {last:?})",
            probe.sensor()
        );
    }
}
