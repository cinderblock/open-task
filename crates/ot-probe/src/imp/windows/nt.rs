//! Hand-declared NT structures.
//!
//! The public SDK (`winternl.h`, and therefore the `windows` crate's metadata) ships
//! `SYSTEM_PROCESS_INFORMATION` with most fields renamed to `Reserved1..7`. The real
//! layout has been stable since Windows XP and is documented in the WDK's `ntddk.h`
//! and in the `phnt` headers that System Informer builds on. We declare it here with
//! its true names so the code that reads it is auditable against those headers, and
//! we pin the layout against the SDK's struct at compile time so a metadata change
//! cannot silently misalign us.

use std::mem::{offset_of, size_of};

use windows::Wdk::System::SystemInformation::SYSTEM_INFORMATION_CLASS;
use windows::Win32::Foundation::NTSTATUS;
use windows::Win32::Foundation::{HANDLE, UNICODE_STRING};
use windows::Win32::System::WindowsProgramming::CLIENT_ID;
use windows::Win32::System::WindowsProgramming::SYSTEM_PROCESS_INFORMATION as SdkSpi;
use windows::Win32::System::WindowsProgramming::SYSTEM_THREAD_INFORMATION as SdkSti;

/// `SystemProcessIdInformation`: the image path of one process, by PID, as an NT
/// device path (`\Device\HarddiskVolume3\Windows\...`), without a handle to the
/// process. Absent from the SDK metadata; the class number and structure are from
/// `phnt`'s `ntexapi.h`, and System Informer relies on them the same way.
pub const SYSTEM_PROCESS_ID_INFORMATION: SYSTEM_INFORMATION_CLASS = SYSTEM_INFORMATION_CLASS(88);

/// In: `ProcessId`, and `ImageName.MaximumLength` / `Buffer` describing a caller
/// buffer. Out: `ImageName` filled in, or `STATUS_INFO_LENGTH_MISMATCH` with
/// `ImageName.MaximumLength` set to the size that would have sufficed.
#[repr(C)]
#[allow(non_snake_case)]
#[derive(Debug, Clone, Copy)]
pub struct SystemProcessIdInformation {
    pub ProcessId: HANDLE,
    pub ImageName: UNICODE_STRING,
}

/// `SYSTEM_PROCESS_INFORMATION` with its real field names.
///
/// Times are in 100 ns units. `KernelTime` and `UserTime` are cumulative for the
/// process. `CreateTime` is a FILETIME. One `SYSTEM_THREAD_INFORMATION` per thread
/// follows each entry in memory; `NextEntryOffset` skips over them.
#[repr(C)]
#[allow(non_snake_case, dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct SystemProcessInformation {
    pub NextEntryOffset: u32,
    pub NumberOfThreads: u32,
    pub WorkingSetPrivateSize: i64,
    pub HardFaultCount: u32,
    pub NumberOfThreadsHighWatermark: u32,
    pub CycleTime: u64,
    pub CreateTime: i64,
    pub UserTime: i64,
    pub KernelTime: i64,
    pub ImageName: UNICODE_STRING,
    pub BasePriority: i32,
    pub UniqueProcessId: HANDLE,
    pub InheritedFromUniqueProcessId: HANDLE,
    pub HandleCount: u32,
    pub SessionId: u32,
    pub UniqueProcessKey: usize,
    pub PeakVirtualSize: usize,
    pub VirtualSize: usize,
    pub PageFaultCount: u32,
    pub PeakWorkingSetSize: usize,
    pub WorkingSetSize: usize,
    pub QuotaPeakPagedPoolUsage: usize,
    pub QuotaPagedPoolUsage: usize,
    pub QuotaPeakNonPagedPoolUsage: usize,
    pub QuotaNonPagedPoolUsage: usize,
    pub PagefileUsage: usize,
    pub PeakPagefileUsage: usize,
    pub PrivatePageCount: usize,
    pub ReadOperationCount: i64,
    pub WriteOperationCount: i64,
    pub OtherOperationCount: i64,
    pub ReadTransferCount: i64,
    pub WriteTransferCount: i64,
    pub OtherTransferCount: i64,
}

// Pin our layout to the SDK's. Every named field the SDK still exposes must sit at
// the same offset, and the total size must agree. If any of these fail, the metadata
// changed underneath us and every other field is suspect.
const _: () = {
    assert!(size_of::<SystemProcessInformation>() == size_of::<SdkSpi>());
    assert!(
        offset_of!(SystemProcessInformation, NextEntryOffset)
            == offset_of!(SdkSpi, NextEntryOffset)
    );
    assert!(
        offset_of!(SystemProcessInformation, NumberOfThreads)
            == offset_of!(SdkSpi, NumberOfThreads)
    );
    assert!(offset_of!(SystemProcessInformation, ImageName) == offset_of!(SdkSpi, ImageName));
    assert!(offset_of!(SystemProcessInformation, BasePriority) == offset_of!(SdkSpi, BasePriority));
    assert!(
        offset_of!(SystemProcessInformation, UniqueProcessId)
            == offset_of!(SdkSpi, UniqueProcessId)
    );
    assert!(offset_of!(SystemProcessInformation, HandleCount) == offset_of!(SdkSpi, HandleCount));
    assert!(offset_of!(SystemProcessInformation, SessionId) == offset_of!(SdkSpi, SessionId));
    assert!(
        offset_of!(SystemProcessInformation, PeakVirtualSize)
            == offset_of!(SdkSpi, PeakVirtualSize)
    );
    assert!(offset_of!(SystemProcessInformation, VirtualSize) == offset_of!(SdkSpi, VirtualSize));
    assert!(
        offset_of!(SystemProcessInformation, PeakWorkingSetSize)
            == offset_of!(SdkSpi, PeakWorkingSetSize)
    );
    assert!(
        offset_of!(SystemProcessInformation, WorkingSetSize) == offset_of!(SdkSpi, WorkingSetSize)
    );
    assert!(
        offset_of!(SystemProcessInformation, QuotaPagedPoolUsage)
            == offset_of!(SdkSpi, QuotaPagedPoolUsage)
    );
    assert!(
        offset_of!(SystemProcessInformation, QuotaNonPagedPoolUsage)
            == offset_of!(SdkSpi, QuotaNonPagedPoolUsage)
    );
    assert!(
        offset_of!(SystemProcessInformation, PagefileUsage) == offset_of!(SdkSpi, PagefileUsage)
    );
    assert!(
        offset_of!(SystemProcessInformation, PeakPagefileUsage)
            == offset_of!(SdkSpi, PeakPagefileUsage)
    );
    assert!(
        offset_of!(SystemProcessInformation, PrivatePageCount)
            == offset_of!(SdkSpi, PrivatePageCount)
    );
    // The SDK's Reserved1[48] must cover exactly the seven fields we name in its place.
    assert!(offset_of!(SystemProcessInformation, WorkingSetPrivateSize) == 8);
    assert!(offset_of!(SystemProcessInformation, KernelTime) + 8 == offset_of!(SdkSpi, ImageName));
    // And Reserved7[6] must be exactly our six I/O counters at the tail.
    assert!(
        offset_of!(SystemProcessInformation, ReadOperationCount) == offset_of!(SdkSpi, Reserved7)
    );
};

/// `SYSTEM_THREAD_INFORMATION` with its real field names. `NumberOfThreads` of these
/// follow each process entry; the SDK hides the three times and the wait time behind
/// `Reserved1..3`. Times are in 100 ns units; `CreateTime` is a FILETIME.
#[repr(C)]
#[allow(non_snake_case, dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct SystemThreadInformation {
    pub KernelTime: i64,
    pub UserTime: i64,
    pub CreateTime: i64,
    pub WaitTime: u32,
    pub StartAddress: *mut core::ffi::c_void,
    pub ClientId: CLIENT_ID,
    pub Priority: i32,
    pub BasePriority: i32,
    pub ContextSwitches: u32,
    pub ThreadState: u32,
    pub WaitReason: u32,
}

const _: () = {
    assert!(size_of::<SystemThreadInformation>() == size_of::<SdkSti>());
    assert!(offset_of!(SystemThreadInformation, KernelTime) == offset_of!(SdkSti, Reserved1));
    assert!(offset_of!(SystemThreadInformation, WaitTime) == offset_of!(SdkSti, Reserved2));
    assert!(offset_of!(SystemThreadInformation, StartAddress) == offset_of!(SdkSti, StartAddress));
    assert!(offset_of!(SystemThreadInformation, ClientId) == offset_of!(SdkSti, ClientId));
    assert!(offset_of!(SystemThreadInformation, Priority) == offset_of!(SdkSti, Priority));
    assert!(offset_of!(SystemThreadInformation, ContextSwitches) == offset_of!(SdkSti, Reserved3));
    assert!(offset_of!(SystemThreadInformation, ThreadState) == offset_of!(SdkSti, ThreadState));
    assert!(offset_of!(SystemThreadInformation, WaitReason) == offset_of!(SdkSti, WaitReason));
};

/// `THREAD_BASIC_INFORMATION`, the `ThreadBasicInformation` class of
/// `NtQueryInformationThread`. Absent from the SDK metadata; layout from `phnt`.
/// Only `TebBaseAddress` is used here.
#[repr(C)]
#[allow(non_snake_case, dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct ThreadBasicInformation {
    pub ExitStatus: NTSTATUS,
    pub TebBaseAddress: *mut core::ffi::c_void,
    pub ClientId: CLIENT_ID,
    pub AffinityMask: usize,
    pub Priority: i32,
    pub BasePriority: i32,
}

impl Default for ThreadBasicInformation {
    fn default() -> Self {
        // SAFETY: all-zero is a valid value for every field (null pointers, zeros).
        unsafe { std::mem::zeroed() }
    }
}

/// Offset of `SubProcessTag` in the TEB: the service tag the service control
/// manager stamps on every thread a service creates. Stable since Vista; the value
/// System Informer and Process Explorer read.
#[cfg(target_pointer_width = "64")]
pub const TEB_SUB_PROCESS_TAG_OFFSET: usize = 0x1720;
#[cfg(target_pointer_width = "32")]
pub const TEB_SUB_PROCESS_TAG_OFFSET: usize = 0x0f60;
