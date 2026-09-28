//! Windows: `WinHTTP` for downloads, the registry for the install, and Setup.
//!
//! `WinHTTP` uses the system's proxy settings (including automatic configuration),
//! the Windows certificate store and TLS stack, and follows redirects (GitHub sends
//! release downloads to another host), refusing any from HTTPS to HTTP. Nothing is
//! bundled for any of it.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
    WINHTTP_QUERY_CONTENT_LENGTH, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY,
};

use crate::{Installation, Platform, Scope, UpdateError};

/// The installer's uninstall entry. The GUID is `AppId` in
/// `installer/windows/open-task.iss`; Inno Setup appends `_is1`.
const UNINSTALL_KEY: PCWSTR = w!(
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{9B1C3F2E-6D7A-4A0E-9B4B-2C8F1E5D7A31}_is1"
);

/// Timeouts in milliseconds: name resolution, connecting, sending, and each wait for
/// data. A stalled download fails instead of hanging the update forever.
const TIMEOUTS_MS: [i32; 4] = [15_000, 15_000, 30_000, 30_000];

pub fn native(user_agent: &str) -> Arc<dyn Platform> {
    Arc::new(Windows {
        agent: HSTRING::from(user_agent),
    })
}

/// Both possible installs: all users (HKLM) and this user (`/CURRENTUSER`, HKCU).
pub fn installations() -> Vec<Installation> {
    [
        (HKEY_LOCAL_MACHINE, Scope::Machine),
        (HKEY_CURRENT_USER, Scope::User),
    ]
    .into_iter()
    .filter_map(|(root, scope)| {
        read_string(root, UNINSTALL_KEY, w!("InstallLocation")).map(|dir| Installation {
            scope,
            dir: PathBuf::from(dir),
        })
    })
    .collect()
}

fn read_string(root: HKEY, key: PCWSTR, name: PCWSTR) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: the buffer and its size are locals that outlive the call; `size` is
    // its length in bytes, as the API wants. The 64-bit view is where the 64-bit
    // installer writes, whatever this process is.
    let status = unsafe {
        RegGetValueW(
            root,
            key,
            name,
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
            None,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            Some(&raw mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let chars = (size as usize / 2).min(buf.len());
    let s = String::from_utf16_lossy(&buf[..chars]);
    Some(s.trim_end_matches('\0').to_owned())
}

struct Windows {
    agent: HSTRING,
}

impl Platform for Windows {
    fn fetch(
        &self,
        url: &str,
        limit: u64,
        sink: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<(), UpdateError> {
        let target = Target::parse(url)
            .ok_or_else(|| UpdateError::Network(format!("not an http(s) URL: {url}")))?;
        // SAFETY (this and the calls below): every handle is checked before use and
        // closed by `Handle`'s drop, children before parents (reverse declaration
        // order); strings are `HSTRING`s or static literals that outlive the calls;
        // out-pointers are locals of the right size.
        let session = Handle::new(
            unsafe {
                WinHttpOpen(
                    &self.agent,
                    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    0,
                )
            },
            url,
        )?;
        let [resolve, connect, send, receive] = TIMEOUTS_MS;
        unsafe { WinHttpSetTimeouts(session.0, resolve, connect, send, receive) }
            .map_err(|e| network(url, &e))?;
        let connection = Handle::new(
            unsafe { WinHttpConnect(session.0, &HSTRING::from(target.host), target.port, 0) },
            url,
        )?;
        let flags = if target.secure {
            WINHTTP_FLAG_SECURE
        } else {
            WINHTTP_OPEN_REQUEST_FLAGS(0)
        };
        let request = Handle::new(
            unsafe {
                WinHttpOpenRequest(
                    connection.0,
                    w!("GET"),
                    &HSTRING::from(target.path),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    std::ptr::null(),
                    flags,
                )
            },
            url,
        )?;
        unsafe { WinHttpSendRequest(request.0, None, None, 0, 0, 0) }
            .map_err(|e| network(url, &e))?;
        unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) }
            .map_err(|e| network(url, &e))?;

        let status = request.number(WINHTTP_QUERY_STATUS_CODE).unwrap_or(0);
        if status != 200 {
            return Err(UpdateError::Http {
                url: url.to_owned(),
                status,
            });
        }
        let total = request.number(WINHTTP_QUERY_CONTENT_LENGTH).map(u64::from);
        let too_large = || UpdateError::TooLarge {
            url: url.to_owned(),
            limit,
        };
        if total.is_some_and(|t| t > limit) {
            return Err(too_large());
        }

        let mut buf = vec![0u8; 64 << 10];
        let mut received = 0u64;
        loop {
            let mut read = 0u32;
            unsafe {
                WinHttpReadData(
                    request.0,
                    buf.as_mut_ptr().cast::<c_void>(),
                    buf.len() as u32,
                    &raw mut read,
                )
            }
            .map_err(|e| network(url, &e))?;
            if read == 0 {
                break;
            }
            received += u64::from(read);
            if received > limit {
                return Err(too_large());
            }
            sink(&buf[..read as usize]).map_err(|source| UpdateError::Io {
                what: "save the download",
                source,
            })?;
            progress(received, total);
        }
        if total.is_some_and(|t| t != received) {
            return Err(UpdateError::Network(format!(
                "{url} ended after {received} of {} bytes",
                total.unwrap_or_default()
            )));
        }
        Ok(())
    }

    fn run_installer(&self, path: &Path, args: &[&str]) -> Result<i32, UpdateError> {
        // Inno Setup's stub asks for no elevation itself (`asInvoker`) and elevates
        // on its own when the install needs it, so a plain CreateProcess works, and
        // Setup knows who the original, unelevated user was.
        let status = std::process::Command::new(path)
            .args(args)
            .status()
            .map_err(|source| UpdateError::Io {
                what: "start Setup",
                source,
            })?;
        Ok(status.code().unwrap_or(-1))
    }
}

/// A `WinHTTP` handle, closed on drop.
struct Handle(*mut c_void);

impl Handle {
    fn new(h: *mut c_void, url: &str) -> Result<Self, UpdateError> {
        if h.is_null() {
            Err(network(url, &windows::core::Error::from_thread()))
        } else {
            Ok(Self(h))
        }
    }

    /// A numeric response header, if present.
    fn number(&self, query: u32) -> Option<u32> {
        let mut value = 0u32;
        let mut size = size_of::<u32>() as u32;
        // SAFETY: a request handle with a received response; the out-pointers are
        // locals and `size` matches.
        unsafe {
            WinHttpQueryHeaders(
                self.0,
                query | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some((&raw mut value).cast::<c_void>()),
                &raw mut size,
                std::ptr::null_mut(),
            )
        }
        .ok()
        .map(|()| value)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a valid handle, closed once.
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// A `WinHTTP` failure, in words. Its error codes have no text in the system message
/// table, so the common ones are spelled out here.
fn network(url: &str, e: &windows::core::Error) -> UpdateError {
    let code = (e.code().0 as u32) & 0xFFFF;
    let what = match code {
        12002 => "the connection timed out".to_owned(),
        12007 => "the server's name could not be resolved (is the network down?)".to_owned(),
        12029 => "could not connect to the server".to_owned(),
        12030 | 12031 => "the connection was lost".to_owned(),
        12157 | 12175 => "the secure connection failed".to_owned(),
        12156 => "a redirect from HTTPS to HTTP was refused".to_owned(),
        _ => format!("network error {code}: {}", e.message()),
    };
    UpdateError::Network(format!("{what} ({url})"))
}

/// The parts of an `http(s)://host[:port]/path` URL that `WinHTTP` wants.
#[derive(Debug, PartialEq, Eq)]
struct Target<'a> {
    secure: bool,
    host: &'a str,
    port: u16,
    path: &'a str,
}

impl<'a> Target<'a> {
    fn parse(url: &'a str) -> Option<Self> {
        let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
            (true, rest)
        } else {
            (false, url.strip_prefix("http://")?)
        };
        let (authority, path) = rest.find('/').map_or((rest, "/"), |i| rest.split_at(i));
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().ok()?),
            None => (authority, if secure { 443 } else { 80 }),
        };
        (!host.is_empty()).then_some(Self {
            secure,
            host,
            port,
            path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_split_into_what_winhttp_wants() {
        assert_eq!(
            Target::parse("https://github.com/cinderblock/open-task/releases/latest"),
            Some(Target {
                secure: true,
                host: "github.com",
                port: 443,
                path: "/cinderblock/open-task/releases/latest",
            })
        );
        assert_eq!(
            Target::parse("http://127.0.0.1:8765/releases"),
            Some(Target {
                secure: false,
                host: "127.0.0.1",
                port: 8765,
                path: "/releases",
            })
        );
        assert_eq!(
            Target::parse("https://example.test").map(|t| t.path),
            Some("/")
        );
        for bad in [
            "ftp://x/y",
            "https://",
            "https://:80/x",
            "https://h:port/x",
            "x",
        ] {
            assert_eq!(Target::parse(bad), None, "{bad}");
        }
    }

    /// Over the network, so not run by default: `cargo test -p ot-update --
    /// --ignored`. A real release asset, through GitHub's redirect to its download
    /// host; a missing one; one over the limit.
    #[test]
    #[ignore = "needs the network"]
    fn fetches_a_release_asset_from_github() {
        let platform = native("open-task-test");
        let url = "https://github.com/cinderblock/open-task/releases/download/v0.2.1/SHA256SUMS";
        let mut body = Vec::new();
        let mut last = (0, None);
        platform
            .fetch(
                url,
                64 << 10,
                &mut |chunk| {
                    body.extend_from_slice(chunk);
                    Ok(())
                },
                &mut |received, total| last = (received, total),
            )
            .unwrap();
        let text = String::from_utf8(body).unwrap();
        assert!(
            text.contains("open-task-v0.2.1-windows-setup.exe"),
            "{text}"
        );
        assert_eq!(last.0, text.len() as u64);

        let missing = platform.fetch(
            &url.replace("SHA256SUMS", "nope"),
            1 << 10,
            &mut |_| Ok(()),
            &mut |_, _| {},
        );
        assert!(
            matches!(missing, Err(UpdateError::Http { status: 404, .. })),
            "{missing:?}"
        );
        let small = platform.fetch(url, 10, &mut |_| Ok(()), &mut |_, _| {});
        assert!(
            matches!(small, Err(UpdateError::TooLarge { .. })),
            "{small:?}"
        );
    }
}
