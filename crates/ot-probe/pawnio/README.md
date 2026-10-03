# PawnIO modules

Signed module blobs for [PawnIO](https://pawnio.eu), the kernel driver open-task
reads package power and CPU temperature through when it is installed. They are
embedded into `ot-probe` with `include_bytes!` by `src/imp/windows/pawnio.rs` and
handed to the driver with `pawnio_load`; the driver checks their signature, so the
bytes must stay exactly as released (`.gitattributes` marks them binary).

| File | Exports used | For |
| --- | --- | --- |
| `IntelMSR.bin` | `ioctl_read_msr` | Intel: RAPL energy counter and the thermal status MSRs |
| `AMDFamily17.bin` | `ioctl_read_msr`, `ioctl_read_smn` | AMD Zen (family 17h, 19h, 1Ah): RAPL energy counter and `THM_TCON_CUR_TMP` |

## Origin

- Repository: https://github.com/namazso/PawnIO.Modules
- Release: [0.2.11](https://github.com/namazso/PawnIO.Modules/releases/tag/0.2.11),
  published 2026-08-30, asset `release_0_2_11.zip`
  (SHA-256 `43608cb89bc84247fef1368a139013f7d043e17db6d6c8dfc9b46bf0905a81f4`)
- `IntelMSR.bin` SHA-256 `d6ed85d65ab17a22f813ef98207d6d537155ee2ded5976a21cb48413c9b92e5f`
- `AMDFamily17.bin` SHA-256 `dae74615761b78bdf064dfb3e136252ddcc6fc727d88f14738d0e5800d427a91`

Both modules refuse every MSR not on their allow-list (`is_allowed_msr_read` in
`IntelMSR.p` and `AMDFamily17.p`); the registers open-task reads are on it.

## Licence

PawnIO.Modules is Copyright (C) 2025 namazso and licensed under the GNU Lesser
General Public License, version 2.1 or later (`SPDX-License-Identifier:
LGPL-2.1-or-later`). The release ships the licence text as `COPYING`; the copy here
is that file, unchanged. The blobs are redistributed unmodified; this notice and the
licence are the attribution the LGPL asks for. The PawnIO driver and `PawnIOLib.dll`
themselves are not shipped: they are GPL-2 with an exception for programs that only
use the device interface, and open-task loads the installed library at run time.
