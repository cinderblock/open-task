//! What this process is allowed to do, asked of the token directly.
//!
//! "Is the token elevated?" (`TokenElevation`) is the wrong question for deciding
//! whether a feature will work. It says whether UAC split the token, and a token
//! built another way can say yes while missing what the feature needs: `runas
//! /trustlevel:0x20000` from an elevated prompt gives a "basic user" token that
//! reports elevated and High integrity, yet has lost the Administrators group and
//! every privilege that matters. So each feature asks about the thing it uses: a
//! privilege the token holds and can enable, or a group it is really a member of.

use std::mem::size_of;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GetLastError, ERROR_NOT_ALL_ASSIGNED, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, CheckTokenMembership, CreateWellKnownSid, GetTokenInformation,
    LookupPrivilegeValueW, TokenPrivileges, LUID_AND_ATTRIBUTES, PSID, SECURITY_MAX_SID_SIZE,
    SE_PRIVILEGE_ENABLED, TOKEN_ACCESS_MASK, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES,
    TOKEN_QUERY, WELL_KNOWN_SID_TYPE,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::tags::OwnedHandle;
use super::AlignedBuf;

fn own_token(access: TOKEN_ACCESS_MASK) -> Option<OwnedHandle> {
    let mut token = HANDLE::default();
    // SAFETY: valid out-pointer; the process pseudo-handle needs no closing.
    unsafe { OpenProcessToken(GetCurrentProcess(), access, &raw mut token) }.ok()?;
    Some(OwnedHandle(token))
}

fn privilege_luid(name: PCWSTR) -> Option<LUID> {
    let mut luid = LUID::default();
    // SAFETY: `name` is a NUL-terminated privilege name; `luid` a valid out-pointer.
    unsafe { LookupPrivilegeValueW(PCWSTR::null(), name, &raw mut luid) }.ok()?;
    Some(luid)
}

/// Turn on a privilege this process's token holds. Administrators' tokens carry
/// most of theirs disabled until asked for. Returns true only if the privilege is
/// now enabled; false if the token does not hold it at all.
pub(super) fn enable_privilege(name: PCWSTR) -> bool {
    let (Some(token), Some(luid)) = (
        own_token(TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY),
        privilege_luid(name),
    ) else {
        return false;
    };
    let tp = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    // SAFETY: `tp` is a valid TOKEN_PRIVILEGES; no previous state is requested.
    let adjusted =
        unsafe { AdjustTokenPrivileges(token.0, false, Some(&raw const tp), 0, None, None) };
    // A token without the privilege still returns success, with the last error set
    // to ERROR_NOT_ALL_ASSIGNED. The `windows` crate's result looks only at the
    // return value, so the last error has to be read here, straight after the call.
    // SAFETY: no arguments; reads this thread's last error.
    adjusted.is_ok() && unsafe { GetLastError() } != ERROR_NOT_ALL_ASSIGNED
}

/// Whether this process's token holds a privilege, enabled or not. Changes nothing.
pub(super) fn holds_privilege(name: PCWSTR) -> bool {
    let (Some(token), Some(luid)) = (own_token(TOKEN_QUERY), privilege_luid(name)) else {
        return false;
    };
    let mut needed = 0u32;
    // SAFETY: a size query with no buffer.
    let _ = unsafe { GetTokenInformation(token.0, TokenPrivileges, None, 0, &raw mut needed) };
    if (needed as usize) < size_of::<TOKEN_PRIVILEGES>() {
        return false;
    }
    let mut buf = AlignedBuf::default();
    buf.resize_bytes(needed as usize);
    // SAFETY: the buffer is the size the first call asked for, and 8-byte aligned.
    let read = unsafe {
        GetTokenInformation(
            token.0,
            TokenPrivileges,
            Some(buf.as_mut_ptr().cast()),
            needed,
            &raw mut needed,
        )
    };
    if read.is_err() {
        return false;
    }
    // SAFETY: the call succeeded: a TOKEN_PRIVILEGES header, then `PrivilegeCount`
    // entries laid out as an array starting at `Privileges`.
    let privileges = unsafe {
        let tp = &*buf.as_ptr().cast::<TOKEN_PRIVILEGES>();
        std::slice::from_raw_parts(tp.Privileges.as_ptr(), tp.PrivilegeCount as usize)
    };
    privileges
        .iter()
        .any(|p| p.Luid.LowPart == luid.LowPart && p.Luid.HighPart == luid.HighPart)
}

/// Whether this process really is a member of a well-known group. A group the token
/// carries only as "deny only" (Administrators in an unelevated administrator's
/// token, or in a basic-user token) does not count.
pub(super) fn in_group(group: WELL_KNOWN_SID_TYPE) -> bool {
    let mut sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut size = SECURITY_MAX_SID_SIZE;
    let psid = PSID(sid.as_mut_ptr().cast());
    // SAFETY: the buffer holds the largest possible SID; `size` says so.
    if unsafe { CreateWellKnownSid(group, None, Some(psid), &raw mut size) }.is_err() {
        return false;
    }
    let mut member = windows::core::BOOL(0);
    // SAFETY: a null token means "this thread's effective token"; `psid` is valid.
    unsafe { CheckTokenMembership(None, psid, &raw mut member) }.is_ok() && member.as_bool()
}

/// Whether this process runs with administrator rights: its token holds the
/// Administrators group for real, which only an elevated token does.
pub(super) fn is_elevated() -> bool {
    in_group(windows::Win32::Security::WinBuiltinAdministratorsSid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::w;
    use windows::Win32::Security::{WinBuiltinAdministratorsSid, WinWorldSid};

    #[test]
    fn a_privilege_the_token_lacks_is_not_reported_enabled() {
        // Only the LSA holds this one; no user token, elevated or not, does.
        assert!(!holds_privilege(w!("SeCreateTokenPrivilege")));
        assert!(!enable_privilege(w!("SeCreateTokenPrivilege")));
        assert!(!enable_privilege(w!("SeNoSuchPrivilege")));
    }

    /// The Windows behavior `enable_privilege` exists to handle: asking to enable a
    /// privilege the token lacks "succeeds", and only the last error says it did
    /// nothing. Trusting the return value alone is what let open-task report
    /// privileges it did not have.
    #[test]
    fn windows_reports_success_for_a_privilege_it_did_not_enable() {
        let token = own_token(TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY).expect("own token");
        let luid = privilege_luid(w!("SeCreateTokenPrivilege")).expect("a real privilege");
        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        // SAFETY: as in `enable_privilege`.
        let (result, last) = unsafe {
            let r = AdjustTokenPrivileges(token.0, false, Some(&raw const tp), 0, None, None);
            (r, GetLastError())
        };
        assert!(result.is_ok(), "the call itself reports success");
        assert_eq!(last, ERROR_NOT_ALL_ASSIGNED);
    }

    #[test]
    fn a_privilege_every_token_holds_is() {
        assert!(holds_privilege(w!("SeChangeNotifyPrivilege")));
        assert!(enable_privilege(w!("SeChangeNotifyPrivilege")));
    }

    #[test]
    fn group_membership_is_real_membership() {
        assert!(in_group(WinWorldSid), "everyone is in Everyone");
        // Administrators counts only when the token can use it, which is exactly
        // when it holds the privileges administrators get.
        assert_eq!(
            in_group(WinBuiltinAdministratorsSid),
            holds_privilege(w!("SeTakeOwnershipPrivilege"))
        );
    }
}
