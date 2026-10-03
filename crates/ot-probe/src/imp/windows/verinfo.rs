//! What an image says about itself: the description, company and product from its
//! version resource, which is where Task Manager's friendly names ("Google
//! Chrome", "Windows Explorer") and Process Explorer's Company column come from.
//!
//! Reading it means `GetFileVersionInfoW`, which opens the file and copies its
//! `VS_VERSIONINFO` block out: a file read, typically a tenth of a millisecond for
//! a cached image and a few milliseconds for one on a cold disk. Callers only do it
//! under the per-pass detail budget, once per process lifetime. Many processes
//! share an image (a dozen `chrome.exe`), so [`VerInfoCache`] keeps one result per
//! path for the life of the probe; an image that changes on disk while running is
//! not a case worth a stat per process.
//!
//! The strings live in a `StringFileInfo` table per language and code page. The
//! `\VarFileInfo\Translation` list says which tables exist; each is tried in turn,
//! and when none names the string (some resources list a translation they never
//! fill in, or list none at all) the three tables nearly every compiler emits are
//! tried as well: US English in Unicode, in the Windows code page, and
//! language-neutral Unicode.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};

use super::AlignedBuf;

/// The strings an image's version resource gives for it. Each is `None` when the
/// resource lacks it, has it empty, or the file could not be read at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct VersionInfo {
    /// `FileDescription`: what the program calls itself.
    pub description: Option<String>,
    /// `CompanyName`: who publishes it.
    pub company: Option<String>,
    /// `ProductName`: the product the file belongs to.
    pub product: Option<String>,
}

/// Translations tried when the resource's own list does not yield a string:
/// US English with the Unicode code page, US English with the Windows-1252 code
/// page, and language-neutral Unicode.
const FALLBACK_TRANSLATIONS: [(u16, u16); 3] = [(0x0409, 0x04B0), (0x0409, 0x04E4), (0, 0x04B0)];

/// The largest version block we will read; a real one is a few kilobytes.
const MAX_BLOCK_BYTES: u32 = 1 << 20;

/// Read the version resource of the image at `path`. Every field is `None` when
/// the file has no resource or cannot be opened.
pub(super) fn version_info(path: &str) -> VersionInfo {
    let mut reader = Reader::default();
    reader.read(path)
}

/// Scratch buffers for one read. Allocated per call: next to the file read that
/// is nothing, and reads happen once per image.
#[derive(Debug, Default)]
struct Reader {
    /// The version block. 8-byte aligned because the block's structures are
    /// `DWORD`-aligned and its strings are UTF-16.
    block: AlignedBuf,
    /// UTF-16 scratch for the path and the sub-block names.
    wide: Vec<u16>,
}

impl Reader {
    fn read(&mut self, path: &str) -> VersionInfo {
        let mut info = VersionInfo::default();
        if !self.load(path) {
            return info;
        }
        let translations = self.translations();
        for (lang, codepage) in translations.iter().chain(FALLBACK_TRANSLATIONS.iter()) {
            if info.description.is_none() {
                info.description = self.string(*lang, *codepage, "FileDescription");
            }
            if info.company.is_none() {
                info.company = self.string(*lang, *codepage, "CompanyName");
            }
            if info.product.is_none() {
                info.product = self.string(*lang, *codepage, "ProductName");
            }
            if info.description.is_some() && info.company.is_some() && info.product.is_some() {
                break;
            }
        }
        info
    }

    /// Copy the file's version block into `self.block`. False when there is none.
    fn load(&mut self, path: &str) -> bool {
        self.wide.clear();
        self.wide.extend(path.encode_utf16());
        self.wide.push(0);
        let name = PCWSTR(self.wide.as_ptr());
        // SAFETY: `name` is NUL-terminated and outlives both calls.
        let size = unsafe { GetFileVersionInfoSizeW(name, None) };
        if size == 0 || size > MAX_BLOCK_BYTES {
            return false;
        }
        self.block.resize_bytes(size as usize);
        // SAFETY: the buffer is `size` bytes, the size the API asked for.
        unsafe { GetFileVersionInfoW(name, None, size, self.block.as_mut_ptr().cast()) }.is_ok()
    }

    /// The `(language, code page)` pairs the resource declares.
    fn translations(&mut self) -> Vec<(u16, u16)> {
        let Some((ptr, len)) = self.query("\\VarFileInfo\\Translation") else {
            return Vec::new();
        };
        let count = len as usize / 4;
        // SAFETY: the API returned `len` bytes of `(WORD, WORD)` pairs inside the
        // block, which stays allocated and unmodified while this slice lives.
        let pairs = unsafe { std::slice::from_raw_parts(ptr.cast::<[u16; 2]>(), count) };
        pairs.iter().map(|p| (p[0], p[1])).collect()
    }

    /// One string of the `StringFileInfo` table for a translation, trimmed; `None`
    /// when absent or blank.
    fn string(&mut self, lang: u16, codepage: u16, key: &str) -> Option<String> {
        let sub = format!("\\StringFileInfo\\{lang:04X}{codepage:04X}\\{key}");
        let (ptr, len) = self.query(&sub)?;
        if len == 0 {
            return None;
        }
        // SAFETY: a string value is `len` UTF-16 units (the count includes the
        // terminator) inside the block, which is unmodified while the slice lives.
        let units = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), len as usize) };
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        let s = String::from_utf16_lossy(&units[..end]);
        let s = s.trim();
        (!s.is_empty()).then(|| s.to_owned())
    }

    /// `VerQueryValueW` for `sub_block`: a pointer into the block and the value's
    /// length (bytes for binary values, UTF-16 units for strings).
    fn query(&mut self, sub_block: &str) -> Option<(*const c_void, u32)> {
        self.wide.clear();
        self.wide.extend(sub_block.encode_utf16());
        self.wide.push(0);
        let mut ptr: *mut c_void = std::ptr::null_mut();
        let mut len = 0u32;
        // SAFETY: the block was filled by GetFileVersionInfoW; the sub-block name is
        // NUL-terminated; `ptr` and `len` are valid out-pointers.
        let ok = unsafe {
            VerQueryValueW(
                self.block.as_ptr().cast(),
                PCWSTR(self.wide.as_ptr()),
                &raw mut ptr,
                &raw mut len,
            )
        };
        (ok.as_bool() && !ptr.is_null()).then_some((ptr.cast_const(), len))
    }
}

/// Version strings by image path, read once per path. The probe keeps one for its
/// life: the set of distinct images on a machine is a few hundred, and a path
/// that could not be read is remembered as empty so it is not retried per process.
#[derive(Debug, Default)]
pub(super) struct VerInfoCache {
    by_path: HashMap<String, Arc<VersionInfo>>,
}

impl VerInfoCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The version strings of the image at `path`, from the cache or the file.
    pub fn get(&mut self, path: &str) -> Arc<VersionInfo> {
        if let Some(v) = self.by_path.get(path) {
            return Arc::clone(v);
        }
        let v = Arc::new(version_info(path));
        self.by_path.insert(path.to_owned(), Arc::clone(&v));
        v
    }

    /// How many distinct images have been read.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.by_path.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system32(name: &str) -> String {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_owned());
        format!("{root}\\System32\\{name}")
    }

    #[test]
    fn explorer_names_itself_and_microsoft() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_owned());
        let v = version_info(&format!("{root}\\explorer.exe"));
        assert_eq!(v.description.as_deref(), Some("Windows Explorer"));
        assert!(
            v.company
                .as_deref()
                .is_some_and(|c| c.contains("Microsoft")),
            "{v:?}"
        );
        assert!(v.product.is_some(), "{v:?}");
    }

    #[test]
    fn notepad_has_a_description() {
        let v = version_info(&system32("notepad.exe"));
        assert!(v.description.is_some(), "{v:?}");
        assert!(
            v.company
                .as_deref()
                .is_some_and(|c| c.contains("Microsoft")),
            "{v:?}"
        );
    }

    #[test]
    fn a_file_without_a_resource_or_without_existing_is_empty() {
        assert_eq!(
            version_info("C:\\no\\such\\file.exe"),
            VersionInfo::default()
        );
        // A text file has no version resource.
        let hosts = format!("{}\\drivers\\etc\\hosts", system32(""));
        assert_eq!(version_info(&hosts), VersionInfo::default());
    }

    #[test]
    fn the_cache_reads_each_path_once_and_shares_the_result() {
        let mut cache = VerInfoCache::new();
        let path = system32("notepad.exe");
        let a = cache.get(&path);
        let b = cache.get(&path);
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(cache.len(), 1);
        let missing = cache.get("C:\\no\\such\\file.exe");
        assert_eq!(*missing, VersionInfo::default());
        assert_eq!(cache.len(), 2);
    }
}
