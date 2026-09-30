//! Build script: the version this build reports, and on Windows the version
//! resource that Explorer shows under Properties > Details, and the app icon.
//!
//! The version comes from git. A clean checkout of a release tag builds as that
//! release (`0.2.1`); anything else carries the commit, git-describe style
//! (`0.2.1-19-g892159c`), with `-dirty` when tracked files differ from it
//! (`0.2.1-0-g892159c-dirty` on a dirty tag). With tags missing (a shallow clone) the
//! Cargo version stands in for the tag (`0.2.1-g892159c`); with no git at all, the
//! Cargo version alone, and a warning. The binary gets it as `OT_VERSION`.
//!
//! Git is only read, never locked: `describe` without `--dirty` and `status` with
//! optional locks off, so a build never collides with a `git add` running beside it.
//!
//! The Windows resources are written here as a `.res` file per binary (the app and
//! its console launcher, [`PROGRAMS`]) and handed to the linker, which converts
//! `.res` inputs itself. No resource compiler is needed. The icon
//! comes from `assets/logo/open-task.ico` (made by `scripts/render-logo.ps1`); a
//! build without it, such as from a packaged crate, gets a warning and no icon.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let cargo_version = env("CARGO_PKG_VERSION");
    let build = describe(&manifest_dir, &cargo_version).unwrap_or_else(|| {
        println!(
            "cargo:warning=git is unavailable; this build reports {cargo_version} without a commit"
        );
        Build {
            version: cargo_version.clone(),
            release: true,
            commits: 0,
        }
    });
    println!("cargo:rustc-env=OT_VERSION={}", build.version);
    watch(&manifest_dir);

    let windows_msvc = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows_msvc {
        // The icon's entries, made once and appended to each binary's resources.
        let ico = workspace(&manifest_dir).join("assets/logo/open-task.ico");
        let mut icon = Vec::new();
        if let Err(e) = std::fs::read(&ico)
            .map_err(|e| e.to_string())
            .and_then(|bytes| icon_res(&bytes, &mut icon))
        {
            println!("cargo:warning=no app icon ({}): {e}", ico.display());
            icon.clear();
        }
        for program in &PROGRAMS {
            let mut resources = version_res(&build, program);
            resources.extend_from_slice(&icon);
            let res = PathBuf::from(env("OUT_DIR")).join(format!("{}.res", program.bin));
            std::fs::write(&res, resources).expect("write the resources");
            println!("cargo:rustc-link-arg-bin={}={}", program.bin, res.display());
        }
    }
}

/// How a binary names itself in its version resource.
struct Program {
    /// The Cargo target.
    bin: &'static str,
    /// What Explorer and Task Manager call it.
    description: &'static str,
    /// The file name it ships under.
    file: &'static str,
}

/// The binaries, each with its own version resource and the app icon.
const PROGRAMS: [Program; 2] = [
    Program {
        bin: "open-task",
        description: "open-task",
        file: "open-task.exe",
    },
    // Renamed when packaged: Cargo cannot emit a `.com` (src/console.rs).
    Program {
        bin: "open-task-console",
        description: "open-task console launcher",
        file: "open-task.com",
    },
];

/// The workspace root, two levels above this crate.
fn workspace(manifest_dir: &Path) -> &Path {
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/ot-app sits two levels under the workspace")
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// What this build is.
struct Build {
    /// Per the module docs.
    version: String,
    /// A tag, clean, nothing after it.
    release: bool,
    /// Commits since the tag; 0 when no tag is known.
    commits: u32,
}

/// The build, per the module docs, or `None` without git.
fn describe(dir: &Path, cargo_version: &str) -> Option<Build> {
    // `--long` makes an exact tag come out as `v0.2.1-0-g…` too, so there is one
    // shape to take apart; `--always` gives just the hash when no tag is reachable.
    let described = git(
        dir,
        &[
            "describe",
            "--tags",
            "--match",
            "v[0-9]*",
            "--long",
            "--always",
            "--abbrev=7",
        ],
    )?;
    let dirty = !git(dir, &["status", "--porcelain", "--untracked-files=no"])?.is_empty();
    let suffix = if dirty { "-dirty" } else { "" };

    let tagged = described.strip_prefix('v').and_then(|rest| {
        let mut parts = rest.rsplitn(3, '-');
        let hash = parts.next()?.strip_prefix('g')?;
        let commits: u32 = parts.next()?.parse().ok()?;
        let tag = parts.next()?;
        Some((tag.to_owned(), commits, hash.to_owned()))
    });
    Some(match tagged {
        Some((tag, 0, _)) if !dirty => {
            if tag != cargo_version {
                println!(
                    "cargo:warning=tag v{tag} does not match the Cargo version {cargo_version}"
                );
            }
            Build {
                version: tag,
                release: true,
                commits: 0,
            }
        }
        Some((tag, commits, hash)) => Build {
            version: format!("{tag}-{commits}-g{hash}{suffix}"),
            release: false,
            commits,
        },
        None => Build {
            version: format!("{cargo_version}-g{described}{suffix}"),
            release: false,
            commits: 0,
        },
    })
}

/// Re-run when the commit, the tags, or the sources change, so the hash and
/// `-dirty` stay true. Paths that do not exist are left out: Cargo would re-run
/// on every build for them.
fn watch(manifest_dir: &Path) {
    let workspace = workspace(manifest_dir);
    let mut paths = vec![
        workspace.join("crates"),
        workspace.join("Cargo.toml"),
        workspace.join("Cargo.lock"),
        workspace.join("assets/logo/open-task.ico"),
    ];
    // In a linked worktree HEAD and the index are its own; refs are shared. Asking
    // git for each path gets both right.
    for name in ["HEAD", "index", "packed-refs", "refs/heads", "refs/tags"] {
        if let Some(p) = git(manifest_dir, &["rev-parse", "--git-path", name]) {
            let p = PathBuf::from(p);
            paths.push(if p.is_absolute() {
                p
            } else {
                manifest_dir.join(p)
            });
        }
    }
    for p in paths.into_iter().filter(|p| p.exists()) {
        println!("cargo:rerun-if-changed={}", p.display());
    }
}

/// `major.minor.patch.build` for the numeric version fields: the tag's numbers, and
/// the commits since the tag as the fourth.
fn numeric(build: &Build) -> [u16; 4] {
    let core = build.version.split(['-', '+']).next().unwrap_or("");
    let mut n = [0u16; 4];
    for (slot, part) in n.iter_mut().zip(core.split('.')) {
        *slot = part.parse().unwrap_or(0);
    }
    n[3] = u16::try_from(build.commits).unwrap_or(u16::MAX);
    n
}

const LANG_EN_US: u16 = 0x0409;
const CODEPAGE_UNICODE: u16 = 1200;
const RT_VERSION: u16 = 16;
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
/// The app icon's group id. Explorer and the taskbar take a program's first icon
/// group; the window loads this one by id (`ot-shell-win`'s `APP_ICON`).
const APP_ICON: u16 = 1;
const VS_FF_PRERELEASE: u32 = 0x2;

/// A `.res` file holding one `VERSIONINFO` resource, for `program`.
///
/// Layout, from the Win32 docs for `RESOURCEHEADER`, `VS_VERSIONINFO`,
/// `StringFileInfo`, `StringTable`, `String`, `VarFileInfo` and `Var`: every
/// structure starts on a 4-byte boundary, its `wLength` covers its header, key,
/// value and children, and a `.res` file begins with an empty entry.
fn version_res(build: &Build, program: &Program) -> Vec<u8> {
    let [a, b, c, d] = numeric(build);
    let ms = (u32::from(a) << 16) | u32::from(b);
    let ls = (u32::from(c) << 16) | u32::from(d);
    let mut dotted = String::new();
    let _ = write!(dotted, "{a}.{b}.{c}.{d}");

    let mut fixed = Vec::with_capacity(52);
    for v in [
        0xFEEF_04BD, // signature
        0x0001_0000, // structure version 1.0
        ms,          // file version
        ls,
        ms, // product version
        ls,
        0x3F, // VS_FFI_FILEFLAGSMASK
        if build.release { 0 } else { VS_FF_PRERELEASE },
        0x0004_0004, // VOS_NT_WINDOWS32
        0x1,         // VFT_APP
        0,           // subtype
        0,           // date
        0,
    ] {
        put_u32(&mut fixed, v);
    }

    let strings = [
        ("CompanyName", "Cameron Tacklind"),
        ("FileDescription", program.description),
        ("FileVersion", dotted.as_str()),
        ("InternalName", program.bin),
        (
            "LegalCopyright",
            "Copyright (c) 2026 Cameron Tacklind. MIT License.",
        ),
        ("OriginalFilename", program.file),
        ("ProductName", "open-task"),
        ("ProductVersion", build.version.as_str()),
        ("Comments", "https://github.com/cinderblock/open-task"),
    ];

    let mut info = Vec::new();
    node(&mut info, "VS_VERSION_INFO", 0, 52, &fixed, |out| {
        node(out, "StringFileInfo", 1, 0, &[], |out| {
            let table = format!("{LANG_EN_US:04x}{CODEPAGE_UNICODE:04x}");
            node(out, &table, 1, 0, &[], |out| {
                for (key, value) in strings {
                    let text = utf16z(value);
                    let chars = (text.len() / 2) as u16;
                    node(out, key, 1, chars, &text, |_| {});
                }
            });
        });
        node(out, "VarFileInfo", 1, 0, &[], |out| {
            let mut translation = Vec::new();
            put_u16(&mut translation, LANG_EN_US);
            put_u16(&mut translation, CODEPAGE_UNICODE);
            node(out, "Translation", 0, 4, &translation, |_| {});
        });
    });

    let mut res = Vec::new();
    res_entry(&mut res, 0, 0, 0, 0, &[]);
    // MOVEABLE | PURE, as rc.exe marks a version resource.
    res_entry(&mut res, RT_VERSION, 1, 0x0030, LANG_EN_US, &info);
    res
}

/// Every image of an `.ico` as an `RT_ICON` (ids 1 to n) and one `RT_GROUP_ICON`
/// ([`APP_ICON`]) listing them, appended to `res`. An `.ico` holds each image just
/// as a resource does (a PNG, or a DIB without its file header); only the
/// directory changes shape: an `.ico` entry ends in the image's file offset, a
/// group entry in its resource id.
fn icon_res(ico: &[u8], res: &mut Vec<u8>) -> Result<(), String> {
    let u16_at = |i: usize| {
        ico.get(i..i + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .ok_or("truncated")
    };
    let u32_at = |i: usize| {
        ico.get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .ok_or("truncated")
    };
    if u16_at(0)? != 0 || u16_at(2)? != 1 {
        return Err("not an icon file".into());
    }
    let count = u16_at(4)?;
    let mut group = Vec::new();
    put_u16(&mut group, 0);
    put_u16(&mut group, 1);
    put_u16(&mut group, count);
    for i in 0..count {
        let entry = 6 + 16 * usize::from(i);
        let (size, offset) = (u32_at(entry + 8)?, u32_at(entry + 12)?);
        let image = ico
            .get(offset..offset + size)
            .ok_or("an image lies outside the file")?;
        let id = i + 1;
        // MOVEABLE | DISCARDABLE, and MOVEABLE | PURE | DISCARDABLE for the group,
        // as rc.exe marks them.
        res_entry(res, RT_ICON, id, 0x1010, LANG_EN_US, image);
        // Width, height, colors, reserved, planes, bit count and size are the same
        // twelve bytes in both directories.
        group.extend_from_slice(&ico[entry..entry + 12]);
        put_u16(&mut group, id);
    }
    res_entry(res, RT_GROUP_ICON, APP_ICON, 0x1030, LANG_EN_US, &group);
    Ok(())
}

/// One version-info structure: `wLength`, `wValueLength`, `wType`, the key, padding,
/// the value, then the children, each on a 4-byte boundary.
fn node(
    out: &mut Vec<u8>,
    key: &str,
    kind: u16,
    value_length: u16,
    value: &[u8],
    children: impl FnOnce(&mut Vec<u8>),
) {
    align4(out);
    let start = out.len();
    put_u16(out, 0);
    put_u16(out, value_length);
    put_u16(out, kind);
    out.extend_from_slice(&utf16z(key));
    align4(out);
    out.extend_from_slice(value);
    children(out);
    let len = u16::try_from(out.len() - start).expect("version resource under 64 KiB");
    out[start..start + 2].copy_from_slice(&len.to_le_bytes());
}

/// A `.res` entry with numeric type and name.
fn res_entry(out: &mut Vec<u8>, kind: u16, name: u16, flags: u16, lang: u16, data: &[u8]) {
    put_u32(out, data.len() as u32);
    put_u32(out, 32); // header size: everything up to the data
    put_u16(out, 0xFFFF);
    put_u16(out, kind);
    put_u16(out, 0xFFFF);
    put_u16(out, name);
    put_u32(out, 0); // data version
    put_u16(out, flags);
    put_u16(out, lang);
    put_u32(out, 0); // version
    put_u32(out, 0); // characteristics
    out.extend_from_slice(data);
    align4(out);
}

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn align4(out: &mut Vec<u8>) {
    out.resize(out.len().next_multiple_of(4), 0);
}
