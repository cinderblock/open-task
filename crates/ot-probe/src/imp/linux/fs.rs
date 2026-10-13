//! Reading `/proc` and `/sys`.
//!
//! Every pass reads a few hundred small pseudo-files. These helpers read into a
//! caller's buffer so a steady-state pass does not allocate per file, and parse
//! the handful of formats those files share.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// Read the whole of `path` into `buf`, replacing its contents.
pub(super) fn read_into(path: impl AsRef<Path>, buf: &mut Vec<u8>) -> io::Result<()> {
    buf.clear();
    File::open(path)?.read_to_end(buf)?;
    Ok(())
}

/// The file's contents with surrounding whitespace removed, or `None` when it
/// cannot be read or is empty.
pub(super) fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

/// The file's contents as a decimal number.
pub(super) fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    read_trimmed(path)?.parse().ok()
}

/// The file's contents as a signed decimal number.
pub(super) fn read_i64(path: impl AsRef<Path>) -> Option<i64> {
    read_trimmed(path)?.parse().ok()
}

/// A `Key:   123 kB` line's value in bytes, as `/proc/meminfo` and
/// `/proc/<pid>/status` write them. Values without a unit are returned as they are.
pub(super) fn kb_value(rest: &[u8]) -> Option<u64> {
    let s = std::str::from_utf8(rest).ok()?.trim();
    let (num, unit) = s.split_once(' ').unwrap_or((s, ""));
    let n: u64 = num.trim().parse().ok()?;
    Some(if unit.trim() == "kB" { n * 1024 } else { n })
}

/// Split a `Key: value` line.
pub(super) fn key_value(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let colon = line.iter().position(|&b| b == b':')?;
    Some((&line[..colon], &line[colon + 1..]))
}

/// A CPU list as the kernel writes it (`0-3,8,10-11`), as indices.
pub(super) fn parse_cpu_list(s: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for part in s.trim().split(',').filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) {
                    out.extend(a..=b);
                }
            }
            None => {
                if let Ok(n) = part.parse() {
                    out.push(n);
                }
            }
        }
    }
    out
}

/// A cache or memory size as sysfs writes it: `32K`, `1024K`, `8M`.
pub(super) fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mult) = match s.as_bytes().last()? {
        b'K' => (&s[..s.len() - 1], 1024),
        b'M' => (&s[..s.len() - 1], 1024 * 1024),
        b'G' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    num.parse::<u64>().ok().map(|n| n * mult)
}

/// Parse an unsigned decimal field, as `/proc` writes them.
pub(super) fn num(field: &[u8]) -> u64 {
    field.iter().fold(0u64, |n, &b| {
        if b.is_ascii_digit() {
            n.wrapping_mul(10).wrapping_add(u64::from(b - b'0'))
        } else {
            n
        }
    })
}

/// Parse a signed decimal field.
pub(super) fn signed(field: &[u8]) -> i64 {
    match field.split_first() {
        Some((b'-', rest)) => -(num(rest).cast_signed()),
        _ => num(field).cast_signed(),
    }
}

/// The last path component of `path`.
pub(super) fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_lists_expand() {
        assert_eq!(parse_cpu_list("0-3,8,10-11\n"), [0, 1, 2, 3, 8, 10, 11]);
        assert_eq!(parse_cpu_list("5"), [5]);
        assert_eq!(parse_cpu_list(""), Vec::<u32>::new());
    }

    #[test]
    fn sizes_scale() {
        assert_eq!(parse_size("32K"), Some(32 * 1024));
        assert_eq!(parse_size("8M\n"), Some(8 << 20));
        assert_eq!(parse_size("512"), Some(512));
        assert_eq!(parse_size(""), None);
    }

    #[test]
    fn kb_values_become_bytes() {
        assert_eq!(kb_value(b"   16318132 kB"), Some(16_318_132 * 1024));
        assert_eq!(kb_value(b"\t7"), Some(7));
    }

    #[test]
    fn numbers_parse() {
        assert_eq!(num(b"12345"), 12345);
        assert_eq!(signed(b"-19"), -19);
        assert_eq!(signed(b"5"), 5);
    }
}
