//! The firmware's description of the machine: the SMBIOS table.
//!
//! `GetSystemFirmwareTable('RSMB', 0)` hands back the whole table the firmware
//! left in memory, as `RawSMBIOSData`: an 8-byte header (calling method, major and
//! minor version, DMI revision, byte length) followed by the structures. It needs
//! no privilege and no WMI; `Win32_PhysicalMemory` and `Win32_BIOS` are this same
//! table read through a COM service, slower by two orders of magnitude. Reading
//! and parsing the table costs about half a millisecond here (`smbios_cost` in the
//! tests measures it), so it is done on demand and nothing is cached.
//!
//! Each structure is a 4-byte header (type, formatted length, handle), a formatted
//! area of that length, then its strings: each NUL-terminated, the set closed by a
//! second NUL. Formatted fields refer to strings by 1-based index. The whole table
//! is parsed once into [`Smbios`] with every offset checked, so firmware that lies
//! about a length cannot make this panic; a short structure just loses the fields
//! past its end.
//!
//! Only four structure types are read: BIOS (0), system (1), baseboard (2) and
//! memory devices (17). Field offsets are from the SMBIOS 3.6 specification.

use ot_model::system::MemoryDevice;
use ot_model::Bytes;
use windows::Win32::System::SystemInformation::{GetSystemFirmwareTable, RSMB};

/// `RawSMBIOSData`'s header, before the structures.
const HEADER_LEN: usize = 8;
/// Structure header: type, length, handle.
const STRUCT_HEADER_LEN: usize = 4;
/// The end-of-table structure.
const TYPE_END: u8 = 127;

/// Strings firmware puts where it has nothing to say.
const PLACEHOLDERS: [&str; 10] = [
    "To Be Filled By O.E.M.",
    "To be filled by O.E.M.",
    "Not Specified",
    "Default string",
    "Unknown",
    "None",
    "Undefined",
    "Not Available",
    "N/A",
    "O.E.M.",
];

/// One structure: its formatted area and its strings, in table order.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Structure {
    kind: u8,
    formatted: Vec<u8>,
    strings: Vec<String>,
}

impl Structure {
    fn u8(&self, offset: usize) -> Option<u8> {
        self.formatted.get(offset).copied()
    }

    fn u16(&self, offset: usize) -> Option<u16> {
        let b = self.formatted.get(offset..offset + 2)?;
        Some(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&self, offset: usize) -> Option<u32> {
        let b = self.formatted.get(offset..offset + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// The string the byte at `offset` refers to, cleaned; `None` for index 0, a
    /// dangling index, or a placeholder.
    fn string(&self, offset: usize) -> Option<String> {
        let index = usize::from(self.u8(offset)?).checked_sub(1)?;
        clean(self.strings.get(index)?)
    }
}

/// The BIOS, from structure type 0.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct Bios {
    pub vendor: Option<String>,
    pub version: Option<String>,
    pub release_date: Option<String>,
}

/// The system, from structure type 1.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct SystemInfo {
    pub manufacturer: Option<String>,
    pub product: Option<String>,
}

/// The baseboard, from structure type 2.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct Baseboard {
    pub manufacturer: Option<String>,
    pub product: Option<String>,
}

/// The parsed table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Smbios {
    major: u8,
    minor: u8,
    structures: Vec<Structure>,
}

impl Smbios {
    /// Read and parse the firmware's table. `None` when the system has none (a
    /// virtual machine without SMBIOS emulation) or the call fails.
    pub fn read() -> Option<Self> {
        // SAFETY: a null buffer asks for the size; no memory is written.
        let needed = unsafe { GetSystemFirmwareTable(RSMB, 0, None) };
        if needed == 0 {
            tracing::warn!("GetSystemFirmwareTable(RSMB) reports no table");
            return None;
        }
        let mut raw = vec![0u8; needed as usize];
        // SAFETY: the buffer is exactly the size the first call asked for.
        let written = unsafe { GetSystemFirmwareTable(RSMB, 0, Some(&mut raw)) };
        if written == 0 || written as usize > raw.len() {
            tracing::warn!(needed, written, "GetSystemFirmwareTable(RSMB) failed");
            return None;
        }
        raw.truncate(written as usize);
        Self::parse(&raw)
    }

    /// Parse a `RawSMBIOSData` blob. Any malformed part ends the parse where it
    /// is; what came before is kept.
    pub fn parse(raw: &[u8]) -> Option<Self> {
        let header = raw.get(..HEADER_LEN)?;
        let major = header[1];
        let minor = header[2];
        let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let body = raw.get(HEADER_LEN..)?;
        let body = &body[..length.min(body.len())];
        Some(Self {
            major,
            minor,
            structures: parse_structures(body),
        })
    }

    /// The table's version, `(major, minor)`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn version(&self) -> (u8, u8) {
        (self.major, self.minor)
    }

    fn first(&self, kind: u8) -> Option<&Structure> {
        self.structures.iter().find(|s| s.kind == kind)
    }

    pub fn bios(&self) -> Option<Bios> {
        let s = self.first(0)?;
        Some(Bios {
            vendor: s.string(0x04),
            version: s.string(0x05),
            release_date: s.string(0x08),
        })
    }

    pub fn system(&self) -> Option<SystemInfo> {
        let s = self.first(1)?;
        Some(SystemInfo {
            manufacturer: s.string(0x04),
            product: s.string(0x05),
        })
    }

    pub fn baseboard(&self) -> Option<Baseboard> {
        let s = self.first(2)?;
        Some(Baseboard {
            manufacturer: s.string(0x04),
            product: s.string(0x05),
        })
    }

    /// Every memory slot the firmware describes, populated or not, in table order.
    pub fn memory_devices(&self) -> Vec<MemoryDevice> {
        self.structures
            .iter()
            .filter(|s| s.kind == 17)
            .map(memory_device)
            .collect()
    }

    /// Memory slots on the board: the number of type 17 structures.
    pub fn memory_slots(&self) -> Option<u32> {
        let n = self.structures.iter().filter(|s| s.kind == 17).count();
        (n > 0).then_some(n as u32)
    }
}

/// Walk the structures until the end-of-table structure, the end of the bytes, or
/// something that cannot be a structure.
fn parse_structures(body: &[u8]) -> Vec<Structure> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(header) = body.get(pos..pos + STRUCT_HEADER_LEN) {
        let kind = header[0];
        let length = usize::from(header[1]);
        if length < STRUCT_HEADER_LEN {
            break;
        }
        let Some(formatted) = body.get(pos..pos + length) else {
            break;
        };
        let mut strings = Vec::new();
        let mut p = pos + length;
        while let Some(rest) = body.get(p..) {
            let Some(nul) = rest.iter().position(|&b| b == 0) else {
                // No terminator: the table ends mid-string. Keep what is there.
                p = body.len();
                break;
            };
            let s = &rest[..nul];
            p += nul + 1;
            if s.is_empty() {
                break;
            }
            strings.push(String::from_utf8_lossy(s).into_owned());
        }
        // A structure with no strings ends in a bare double NUL, of which the loop
        // above consumed one.
        if strings.is_empty() && body.get(p) == Some(&0) {
            p += 1;
        }
        out.push(Structure {
            kind,
            formatted: formatted.to_vec(),
            strings,
        });
        if kind == TYPE_END {
            break;
        }
        pos = p;
    }
    out
}

/// Decode a type 17 structure.
fn memory_device(s: &Structure) -> MemoryDevice {
    let size = s.u16(0x0C).and_then(|raw| match raw {
        0 | 0xFFFF => None,
        0x7FFF => s
            .u32(0x1C)
            .map(|mb| Bytes(u64::from(mb & 0x7FFF_FFFF) << 20)),
        r if r & 0x8000 != 0 => Some(Bytes(u64::from(r & 0x7FFF) << 10)),
        r => Some(Bytes(u64::from(r) << 20)),
    });
    // Configured speed (2.7+), falling back to the rated speed. 0xFFFF in either
    // means "see the extended field" (3.3+).
    let speed = |short: usize, extended: usize| match s.u16(short) {
        Some(0) | None => None,
        Some(0xFFFF) => s.u32(extended).filter(|&v| v > 0),
        Some(v) => Some(u32::from(v)),
    };
    let speed_mts = speed(0x20, 0x58).or_else(|| speed(0x15, 0x54));
    MemoryDevice {
        slot: s.string(0x10).unwrap_or_default(),
        size,
        speed_mts,
        form_factor: s.u8(0x0E).and_then(form_factor).map(str::to_owned),
        kind: s.u8(0x12).and_then(memory_type).map(str::to_owned),
        manufacturer: s.string(0x17),
        part_number: s.string(0x1A),
    }
}

/// SMBIOS 3.6, table 76. Unknown and "other" are left empty.
fn form_factor(code: u8) -> Option<&'static str> {
    Some(match code {
        0x03 => "SIMM",
        0x04 => "SIP",
        0x05 => "Chip",
        0x06 => "DIP",
        0x07 => "ZIP",
        0x08 => "Proprietary card",
        0x09 => "DIMM",
        0x0A => "TSOP",
        0x0B => "Row of chips",
        0x0C => "RIMM",
        0x0D => "SODIMM",
        0x0E => "SRIMM",
        0x0F => "FB-DIMM",
        0x10 => "Die",
        _ => return None,
    })
}

/// SMBIOS 3.6, table 77. Unknown and "other" are left empty.
fn memory_type(code: u8) -> Option<&'static str> {
    Some(match code {
        0x03 => "DRAM",
        0x04 => "EDRAM",
        0x05 => "VRAM",
        0x06 => "SRAM",
        0x07 => "RAM",
        0x08 => "ROM",
        0x09 => "Flash",
        0x0A => "EEPROM",
        0x0B => "FEPROM",
        0x0C => "EPROM",
        0x0D => "CDRAM",
        0x0E => "3DRAM",
        0x0F => "SDRAM",
        0x10 => "SGRAM",
        0x11 => "RDRAM",
        0x12 => "DDR",
        0x13 => "DDR2",
        0x14 => "DDR2 FB-DIMM",
        0x18 => "DDR3",
        0x19 => "FBD2",
        0x1A => "DDR4",
        0x1B => "LPDDR",
        0x1C => "LPDDR2",
        0x1D => "LPDDR3",
        0x1E => "LPDDR4",
        0x1F => "Logical non-volatile device",
        0x20 => "HBM",
        0x21 => "HBM2",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        0x24 => "HBM3",
        _ => return None,
    })
}

/// Trim, and drop the strings firmware uses for "nothing".
fn clean(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || PLACEHOLDERS.iter().any(|p| p.eq_ignore_ascii_case(s)) {
        return None;
    }
    Some(s.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Append one structure: header, formatted bytes, strings, terminator.
    fn push(table: &mut Vec<u8>, kind: u8, handle: u16, formatted: &[u8], strings: &[&str]) {
        let length = (STRUCT_HEADER_LEN + formatted.len()) as u8;
        table.extend_from_slice(&[kind, length]);
        table.extend_from_slice(&handle.to_le_bytes());
        table.extend_from_slice(formatted);
        for s in strings {
            table.extend_from_slice(s.as_bytes());
            table.push(0);
        }
        if strings.is_empty() {
            table.push(0);
        }
        table.push(0);
    }

    /// A type 17 formatted area (after the 4-byte header) of SMBIOS 3.2 length.
    fn memory_formatted(
        size: u16,
        form: u8,
        kind: u8,
        rated: u16,
        configured: u16,
        extended: u32,
    ) -> Vec<u8> {
        let mut f = vec![0u8; 0x5C - STRUCT_HEADER_LEN];
        let at = |f: &mut Vec<u8>, off: usize, bytes: &[u8]| {
            f[off - STRUCT_HEADER_LEN..off - STRUCT_HEADER_LEN + bytes.len()]
                .copy_from_slice(bytes);
        };
        at(&mut f, 0x0C, &size.to_le_bytes());
        at(&mut f, 0x0E, &[form]);
        at(&mut f, 0x10, &[1]); // device locator
        at(&mut f, 0x12, &[kind]);
        at(&mut f, 0x15, &rated.to_le_bytes());
        at(&mut f, 0x17, &[2]); // manufacturer
        at(&mut f, 0x1A, &[3]); // part number
        at(&mut f, 0x1C, &extended.to_le_bytes());
        at(&mut f, 0x20, &configured.to_le_bytes());
        f
    }

    fn table(structures: &[u8]) -> Vec<u8> {
        let mut raw = vec![0u8, 3, 6, 0];
        raw.extend_from_slice(&(structures.len() as u32).to_le_bytes());
        raw.extend_from_slice(structures);
        raw
    }

    #[test]
    fn a_hand_built_table_decodes() {
        let mut body = Vec::new();
        // Type 0: vendor @4, version @5, start segment @6..8, date @8.
        push(
            &mut body,
            0,
            0x0000,
            &[1, 2, 0, 0, 3],
            &["American Megatrends", "F4", "03/15/2024"],
        );
        // Type 1 with placeholder strings.
        push(
            &mut body,
            1,
            0x0001,
            &[1, 2],
            &["To Be Filled By O.E.M.", "  Z790 AORUS ELITE  "],
        );
        // One populated DDR5 SODIMM, 16 GB, rated 6400, configured 5600.
        let populated = memory_formatted(16 * 1024, 0x0D, 0x22, 6400, 5600, 0);
        push(
            &mut body,
            17,
            0x0011,
            &populated[..],
            &["DIMM A1", "Micron", "MTC8C1084S1SC56BG1 "],
        );
        // One empty slot.
        let empty = memory_formatted(0, 0x0D, 0x02, 0, 0, 0);
        push(
            &mut body,
            17,
            0x0012,
            &empty[..],
            &["DIMM B1", "Unknown", "None"],
        );
        push(&mut body, TYPE_END, 0x00FF, &[], &[]);
        // Trailing garbage after the end marker is not read.
        body.extend_from_slice(&[17, 1, 2]);

        let s = Smbios::parse(&table(&body)).expect("parses");
        assert_eq!(s.version(), (3, 6));
        assert_eq!(s.structures.len(), 5);
        assert_eq!(
            s.bios(),
            Some(Bios {
                vendor: Some("American Megatrends".into()),
                version: Some("F4".into()),
                release_date: Some("03/15/2024".into()),
            })
        );
        assert_eq!(
            s.system(),
            Some(SystemInfo {
                manufacturer: None,
                product: Some("Z790 AORUS ELITE".into()),
            })
        );
        assert_eq!(s.baseboard(), None);
        assert_eq!(s.memory_slots(), Some(2));
        let devices = s.memory_devices();
        assert_eq!(
            devices,
            vec![
                MemoryDevice {
                    slot: "DIMM A1".into(),
                    size: Some(Bytes(16 << 30)),
                    speed_mts: Some(5600),
                    form_factor: Some("SODIMM".into()),
                    kind: Some("DDR5".into()),
                    manufacturer: Some("Micron".into()),
                    part_number: Some("MTC8C1084S1SC56BG1".into()),
                },
                MemoryDevice {
                    slot: "DIMM B1".into(),
                    size: None,
                    speed_mts: None,
                    form_factor: Some("SODIMM".into()),
                    kind: None,
                    manufacturer: None,
                    part_number: None,
                },
            ]
        );
    }

    #[test]
    fn sizes_use_every_encoding() {
        let device = |size: u16, extended: u32| {
            let mut body = Vec::new();
            let f = memory_formatted(size, 0x09, 0x1A, 3200, 0, extended);
            push(&mut body, 17, 1, &f[..], &["A", "B", "C"]);
            Smbios::parse(&table(&body))
                .expect("parses")
                .memory_devices()
                .remove(0)
        };
        // Kilobyte granularity, extended size, unknown size, rated-speed fallback.
        assert_eq!(device(0x8000 | 512, 0).size, Some(Bytes(512 << 10)));
        assert_eq!(device(0x7FFF, 65536).size, Some(Bytes(64 << 30)));
        assert_eq!(device(0xFFFF, 0).size, None);
        assert_eq!(device(8192, 0).speed_mts, Some(3200));
    }

    #[test]
    fn malformed_tables_do_not_panic() {
        assert!(Smbios::parse(&[]).is_none());
        assert!(Smbios::parse(&[0; 7]).is_none());
        // Header claiming more than is there.
        let mut raw = vec![0, 3, 0, 0, 0xFF, 0xFF, 0, 0];
        raw.extend_from_slice(&[17, 0x40, 1, 0, 9, 9]);
        let s = Smbios::parse(&raw).expect("parses");
        assert!(s.structures.is_empty(), "{s:?}");
        // A structure whose length is shorter than its header.
        let s = Smbios::parse(&table(&[17, 2, 0, 0, 0, 0])).expect("parses");
        assert_eq!(s.structures, Vec::new());
        // A short type 17 with dangling string indexes and no terminator.
        let s = Smbios::parse(&table(&[17, 6, 0, 0, 0, 7, b'X'])).expect("parses");
        let d = s.memory_devices();
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].slot, "");
        assert_eq!(d[0].size, None);
        // Type 0 that is only a header: every field absent.
        let s = Smbios::parse(&table(&[0, 4, 0, 0, 0, 0])).expect("parses");
        assert_eq!(s.bios(), Some(Bios::default()));
    }

    /// What one read and parse costs. Ignored by default because it measures the
    /// machine it runs on:
    ///
    /// ```text
    /// cargo test -p ot-probe --release smbios_cost -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "measures this machine; run by hand"]
    fn smbios_cost() {
        let n = 200u32;
        let start = std::time::Instant::now();
        let mut devices = 0;
        for _ in 0..n {
            devices = Smbios::read()
                .expect("an SMBIOS table")
                .memory_devices()
                .len();
        }
        let mean = start.elapsed().as_secs_f64() * 1e6 / f64::from(n);
        println!("Smbios::read() + memory_devices(): mean {mean:.1} us ({devices} devices)");
    }

    #[test]
    fn this_machine_has_firmware_and_memory() {
        let s = Smbios::read().expect("an SMBIOS table");
        assert!(s.version().0 >= 2, "{:?}", s.version());
        let devices = s.memory_devices();
        assert!(
            devices.iter().any(|d| d.size.is_some()),
            "a running machine has memory: {devices:?}"
        );
        assert!(
            s.bios().is_some_and(|b| b.vendor.is_some()),
            "{:?}",
            s.bios()
        );
    }
}
