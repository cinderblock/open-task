//! Windows performance counters (PDH), for the numbers no direct API gives an
//! unelevated caller.
//!
//! - **The processors' current clock.** `% Processor Performance` of each logical
//!   processor, times the base clock, is how Task Manager computes "Speed". The
//!   older `CallNtPowerInformation(...).CurrentMhz` stays at the base clock while the
//!   cores boost, so it is not used.
//! - **The memory lists**: modified, standby (all three priorities), free and zero.
//!   `SystemMemoryListInformation` has them too, but needs a privilege an unelevated
//!   process does not hold; the counters do not.
//!
//! Counters are added by their English names (`PdhAddEnglishCounterW`), so this
//! works on any display language. One query holds them all and is collected once
//! per sampling pass, on the sampler thread. Rate counters need two collections
//! before they have a value, so the first pass reports no clock. A machine whose
//! counter registry is damaged simply gets no counters; nothing else depends on them.

use ot_model::Bytes;
use windows::core::{w, PCWSTR};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhGetFormattedCounterValue, PdhOpenQueryW, PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA,
    PDH_FMT, PDH_FMT_COUNTERVALUE, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_FMT_LARGE,
    PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
};

use super::AlignedBuf;

/// `PDH_FMT_NOCAP100` is missing from the SDK metadata. Without it, a percentage
/// counter is clamped at 100, and a boosting core runs well above 100% of base.
const PDH_FMT_NOCAP100: u32 = 0x0000_8000;
const ERROR_SUCCESS: u32 = 0;

/// The physical memory lists, from one collection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct MemoryLists {
    pub modified: Option<Bytes>,
    pub standby: Option<Bytes>,
    pub free: Option<Bytes>,
}

/// An open PDH query and the counters in it.
#[derive(Debug)]
pub(super) struct PerfCounters {
    query: PDH_HQUERY,
    performance: Option<PDH_HCOUNTER>,
    modified: Option<PDH_HCOUNTER>,
    /// Core, normal priority, reserve: standby is their sum.
    standby: [Option<PDH_HCOUNTER>; 3],
    free: Option<PDH_HCOUNTER>,
    /// Scratch for instance arrays, reused every pass.
    items: AlignedBuf,
}

// SAFETY: PDH query and counter handles may be used from any thread, just not from
// two at once. `PerfCounters` belongs to the probe, which one sampler thread owns.
unsafe impl Send for PerfCounters {}

impl Drop for PerfCounters {
    fn drop(&mut self) {
        // SAFETY: the query came from PdhOpenQueryW and is closed exactly once; that
        // also frees every counter in it.
        unsafe {
            let _ = PdhCloseQuery(self.query);
        }
    }
}

impl PerfCounters {
    /// Open the query and add every counter that exists here. `None` only if PDH
    /// itself will not open a query.
    pub fn open() -> Option<Self> {
        let mut query = PDH_HQUERY::default();
        // SAFETY: a null data source means live data; `query` is a valid out-pointer.
        if unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &raw mut query) } != ERROR_SUCCESS {
            return None;
        }
        let mut c = Self {
            query,
            performance: None,
            modified: None,
            standby: [None; 3],
            free: None,
            items: AlignedBuf::default(),
        };
        c.performance = c.add(w!(r"\Processor Information(*)\% Processor Performance"));
        c.modified = c.add(w!(r"\Memory\Modified Page List Bytes"));
        c.standby = [
            c.add(w!(r"\Memory\Standby Cache Core Bytes")),
            c.add(w!(r"\Memory\Standby Cache Normal Priority Bytes")),
            c.add(w!(r"\Memory\Standby Cache Reserve Bytes")),
        ];
        c.free = c.add(w!(r"\Memory\Free & Zero Page List Bytes"));
        Some(c)
    }

    fn add(&self, path: PCWSTR) -> Option<PDH_HCOUNTER> {
        let mut counter = PDH_HCOUNTER::default();
        // SAFETY: the query is open; the path is a static wide string.
        let status = unsafe { PdhAddEnglishCounterW(self.query, path, 0, &raw mut counter) };
        (status == ERROR_SUCCESS).then_some(counter)
    }

    /// Whether the clock can be reported at all.
    pub fn has_clock(&self) -> bool {
        self.performance.is_some()
    }

    /// Take this pass's readings. Returns false if the collection failed.
    pub fn collect(&mut self) -> bool {
        // SAFETY: the query is open.
        unsafe { PdhCollectQueryData(self.query) == ERROR_SUCCESS }
    }

    /// Each logical processor's performance as a percentage of its base clock,
    /// written into `out[index]` for the processors of group 0. Entries without a
    /// reading this pass are left as they were.
    pub fn processor_performance(&mut self, out: &mut [Option<f64>]) {
        let Some(counter) = self.performance else {
            return;
        };
        let format = PDH_FMT(PDH_FMT_DOUBLE.0 | PDH_FMT_NOCAP100);
        let Some(items) = self.array(counter, format) else {
            return;
        };
        for item in items {
            if !valid(item.FmtValue.CStatus) {
                continue;
            }
            // SAFETY: PDH points `szName` at a string inside the same buffer.
            let name = unsafe { item.szName.to_string() }.unwrap_or_default();
            if let Some(i) = group0_index(&name) {
                if let Some(slot) = out.get_mut(i) {
                    // SAFETY: a DOUBLE format fills the `doubleValue` member.
                    *slot = Some(unsafe { item.FmtValue.Anonymous.doubleValue });
                }
            }
        }
    }

    /// The memory lists, where their counters exist and have a value.
    pub fn memory(&self) -> MemoryLists {
        let bytes = |c: Option<PDH_HCOUNTER>| c.and_then(large).map(|v| Bytes(v.max(0) as u64));
        let standby = self
            .standby
            .iter()
            .map(|&c| bytes(c))
            .try_fold(0u64, |sum, b| b.map(|b| sum + b.get()))
            .map(Bytes);
        MemoryLists {
            modified: bytes(self.modified),
            standby,
            free: bytes(self.free),
        }
    }

    /// A wildcard counter's instances, in the reused scratch buffer.
    fn array(
        &mut self,
        counter: PDH_HCOUNTER,
        format: PDH_FMT,
    ) -> Option<&[PDH_FMT_COUNTERVALUE_ITEM_W]> {
        loop {
            let mut size = self.items.len_bytes() as u32;
            let mut count = 0u32;
            let buffer = (size > 0).then(|| self.items.as_mut_ptr().cast());
            // SAFETY: `size` is the buffer's length in bytes (zero asks for the size).
            let status = unsafe {
                PdhGetFormattedCounterArrayW(counter, format, &raw mut size, &raw mut count, buffer)
            };
            match status {
                ERROR_SUCCESS => {
                    // SAFETY: on success the buffer starts with `count` items; the
                    // strings they point at follow them in the same buffer.
                    return Some(unsafe {
                        std::slice::from_raw_parts(
                            self.items.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(),
                            count as usize,
                        )
                    });
                }
                PDH_MORE_DATA if (size as usize) > self.items.len_bytes() => {
                    self.items.resize_bytes(size as usize);
                }
                _ => return None,
            }
        }
    }
}

fn valid(status: u32) -> bool {
    status == PDH_CSTATUS_VALID_DATA || status == PDH_CSTATUS_NEW_DATA
}

fn large(counter: PDH_HCOUNTER) -> Option<i64> {
    let mut value = PDH_FMT_COUNTERVALUE::default();
    // SAFETY: `value` is a valid out-struct; the type out-pointer is optional.
    let status =
        unsafe { PdhGetFormattedCounterValue(counter, PDH_FMT_LARGE, None, &raw mut value) };
    if status != ERROR_SUCCESS || !valid(value.CStatus) {
        return None;
    }
    // SAFETY: a LARGE format fills the `largeValue` member.
    Some(unsafe { value.Anonymous.largeValue })
}

/// `Processor Information` names its instances `group,number`, plus totals such as
/// `_Total` and `0,_Total`. Only group 0 maps onto our processor list for now (the
/// probe samples one processor group; see the module docs of `windows`).
fn group0_index(name: &str) -> Option<usize> {
    let (group, number) = name.split_once(',')?;
    if group != "0" {
        return None;
    }
    number.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names_map_to_group_zero_processors() {
        assert_eq!(group0_index("0,0"), Some(0));
        assert_eq!(group0_index("0,11"), Some(11));
        assert_eq!(group0_index("0,_Total"), None);
        assert_eq!(group0_index("_Total"), None);
        assert_eq!(group0_index("1,3"), None);
    }

    #[test]
    fn this_machine_has_clock_and_memory_list_counters() {
        let mut c = PerfCounters::open().expect("PDH opens a query");
        assert!(c.has_clock());
        assert!(c.collect());
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(c.collect());
        let mut perf = vec![None; 64];
        c.processor_performance(&mut perf);
        let read = perf.iter().flatten().count();
        assert!(read >= 1, "{perf:?}");
        assert!(
            perf.iter().flatten().all(|&p| p > 0.0 && p < 500.0),
            "{perf:?}"
        );
        let m = c.memory();
        assert!(
            m.standby.is_some() && m.free.is_some() && m.modified.is_some(),
            "{m:?}"
        );
    }
}
