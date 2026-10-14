//! Loading a toolkit plugin.
//!
//! A release binary carries its plugins inside it. Each is written to an
//! anonymous file (`memfd_create`) and loaded from `/proc/self/fd/N`: nothing
//! touches the disk, so there is no stale copy to clean up, no race with another
//! instance, and no trouble with a `noexec` `/tmp`. A development build has none
//! embedded and loads them from beside the executable, where `cargo build` puts
//! them.
//!
//! A plugin is never unloaded: toolkits register types and threads that do not
//! survive `dlclose`, and the process ends with the window anyway.

use std::ffi::{c_void, CStr, CString};
use std::io::Write as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::path::Path;

use ot_shell_abi::{Plugin, PluginFn, ABI_VERSION, PLUGIN_SYMBOL};

use crate::select::Toolkit;

/// A plugin compiled into the binary.
#[derive(Debug, Clone, Copy)]
pub struct Embedded {
    pub toolkit: Toolkit,
    pub bytes: &'static [u8],
}

/// A loaded plugin's function table, which lives as long as the process.
#[derive(Debug, Clone, Copy)]
pub struct Loaded {
    pub toolkit: Toolkit,
    pub plugin: &'static Plugin,
}

/// Where a toolkit's plugin comes from: the binary, or a file beside it.
#[must_use]
pub fn source(toolkit: Toolkit, embedded: &[Embedded]) -> Option<Source> {
    if let Some(e) = embedded.iter().find(|e| e.toolkit == toolkit) {
        return Some(Source::Embedded(*e));
    }
    let exe = std::env::current_exe().ok()?;
    let path = exe.parent()?.join(toolkit.library());
    path.is_file().then_some(Source::File(path))
}

#[derive(Debug, Clone)]
pub enum Source {
    Embedded(Embedded),
    File(std::path::PathBuf),
}

impl Source {
    /// Load the plugin and check that it speaks this build's ABI.
    ///
    /// # Errors
    /// Why not, in words: a missing toolkit library shows here as the dynamic
    /// loader's message (`libgtk-4.so.1: cannot open shared object file`).
    pub fn load(&self, toolkit: Toolkit) -> Result<Loaded, String> {
        let handle = match self {
            Self::Embedded(e) => open_bytes(toolkit, e.bytes)?,
            Self::File(path) => open_path(path)?,
        };
        let plugin = table(handle)?;
        Ok(Loaded { toolkit, plugin })
    }
}

/// `dlerror()`'s message, or a stand-in.
fn dl_error() -> String {
    // SAFETY: dlerror returns NULL or a NUL-terminated string owned by libc,
    // valid until the next dl* call on this thread; it is copied at once.
    unsafe {
        let e = libc::dlerror();
        if e.is_null() {
            "unknown error".to_owned()
        } else {
            CStr::from_ptr(e).to_string_lossy().into_owned()
        }
    }
}

fn open_path(path: &Path) -> Result<*mut c_void, String> {
    let c = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: a NUL-terminated path; the handle is kept for the process's life.
    let handle = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        Err(dl_error())
    } else {
        Ok(handle)
    }
}

fn open_bytes(toolkit: Toolkit, bytes: &[u8]) -> Result<*mut c_void, String> {
    let name =
        CString::new(format!("open-task-{}", toolkit.library())).map_err(|e| e.to_string())?;
    // SAFETY: a NUL-terminated name; the descriptor returned is owned below.
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(format!("memfd_create: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: a fresh descriptor nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut file = std::fs::File::from(fd);
    file.write_all(bytes)
        .map_err(|e| format!("writing the plugin: {e}"))?;
    // The mapping dlopen makes keeps the file alive after the descriptor closes.
    let path = format!("/proc/self/fd/{}", file.as_raw_fd());
    open_path(Path::new(&path))
}

fn table(handle: *mut c_void) -> Result<&'static Plugin, String> {
    // SAFETY: a handle from dlopen and a NUL-terminated symbol name.
    let sym = unsafe { libc::dlsym(handle, PLUGIN_SYMBOL.as_ptr().cast()) };
    if sym.is_null() {
        return Err(format!("not an open-task plugin: {}", dl_error()));
    }
    // SAFETY: the symbol is the plugin's entry point, `OtShellPluginFn`.
    let entry: PluginFn = unsafe { std::mem::transmute::<*mut c_void, PluginFn>(sym) };
    // SAFETY: the entry point takes nothing and returns a static table or NULL.
    let plugin = unsafe { entry() };
    // SAFETY: a non-null result points at a table that lives as long as the
    // library, which is never unloaded. Only the two version fields are read
    // before the version is known to match.
    let plugin = unsafe { plugin.as_ref() }.ok_or("the plugin returned no table")?;
    if plugin.abi_version != ABI_VERSION {
        return Err(format!(
            "the plugin speaks ABI {}, this build {ABI_VERSION}",
            plugin.abi_version
        ));
    }
    if (plugin.struct_size as usize) < size_of::<Plugin>() {
        return Err("the plugin's table is too short".to_owned());
    }
    Ok(plugin)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The anonymous-file path really loads a library: a copy of the C math
    /// library, loaded from memory, computes a cosine.
    #[test]
    fn libraries_load_from_memory() {
        let Some(libm) = [
            "/lib/x86_64-linux-gnu/libm.so.6",
            "/lib/aarch64-linux-gnu/libm.so.6",
            "/usr/lib/libm.so.6",
            "/lib64/libm.so.6",
        ]
        .into_iter()
        .find(|p| Path::new(p).is_file()) else {
            eprintln!("no libm found; skipping");
            return;
        };
        let bytes = std::fs::read(libm).expect("read libm");
        let handle = open_bytes(Toolkit::Gtk, &bytes).expect("load from memory");
        // SAFETY: a valid handle and a NUL-terminated name.
        let sym = unsafe { libc::dlsym(handle, c"cos".as_ptr()) };
        assert!(!sym.is_null());
        // SAFETY: libm's cos.
        let cos: extern "C" fn(f64) -> f64 = unsafe { std::mem::transmute(sym) };
        assert!((cos(0.0) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_library_without_the_entry_point_is_refused() {
        let Some(libm) = [
            "/lib/x86_64-linux-gnu/libm.so.6",
            "/lib/aarch64-linux-gnu/libm.so.6",
            "/usr/lib/libm.so.6",
            "/lib64/libm.so.6",
        ]
        .into_iter()
        .find(|p| Path::new(p).is_file()) else {
            return;
        };
        let err = Source::File(libm.into()).load(Toolkit::Qt).unwrap_err();
        assert!(err.starts_with("not an open-task plugin"), "{err}");
    }
}
