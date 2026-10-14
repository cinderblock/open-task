//! Build script: on Linux, compile `src/plugin.cpp` against Qt 6 Widgets and
//! `ot-shell-abi`'s header, and link Qt. Elsewhere there is nothing to build; the
//! crate is empty.
//!
//! Qt is found with pkg-config where its `.pc` files are installed, else by
//! asking its `qmake` where the headers and libraries are: Ubuntu 22.04's Qt 6.2,
//! the oldest the plugin supports and what releases are built against, ships no
//! `.pc` files.
//!
//! No `moc` step: the C++ uses only functor connections and overridden event
//! handlers, which need no meta-object code.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The Qt modules the plugin uses, in link order.
const MODULES: [&str; 3] = ["Widgets", "Gui", "Core"];

/// Where Qt is, as the compiler and linker need it.
struct Qt {
    include_paths: Vec<PathBuf>,
    defines: Vec<(String, Option<String>)>,
}

fn main() {
    println!("cargo:rerun-if-changed=src/plugin.cpp");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let abi = manifest.join("../ot-shell-abi/include");
    println!(
        "cargo:rerun-if-changed={}",
        abi.join("ot_shell.h").display()
    );
    println!("cargo:rerun-if-env-changed=QMAKE");

    let qt = from_pkg_config().or_else(from_qmake).unwrap_or_else(|e| {
        panic!(
            "Qt 6 Widgets not found ({e}). Install its development files \
             (Debian and Ubuntu: qt6-base-dev qt6-base-dev-tools), or build \
             without this crate: cargo build --workspace --exclude ot-shell-qt"
        )
    });

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .file("src/plugin.cpp")
        .include(&abi)
        .std("c++17")
        // Qt is built with -reduce-relocations; code using it must be
        // position-independent, and its symbols are not exported from here.
        .flag("-fPIC")
        .flag("-fvisibility=hidden")
        .flag_if_supported("-Wno-deprecated-declarations")
        .warnings(true);
    for dir in &qt.include_paths {
        build.include(dir);
    }
    for (name, value) in &qt.defines {
        build.define(name, value.as_deref());
    }
    build.compile("ot_shell_qt_cpp");
}

/// Qt through its pkg-config files, which also tell Cargo what to link.
fn from_pkg_config() -> Result<Qt, String> {
    let lib = pkg_config::Config::new()
        .atleast_version("6.2")
        .probe("Qt6Widgets")
        .map_err(|e| e.to_string())?;
    Ok(Qt {
        include_paths: lib.include_paths,
        defines: lib.defines.into_iter().collect(),
    })
}

/// Qt through `qmake -query`: its version, headers and libraries.
fn from_qmake(pkg_config_error: String) -> Result<Qt, String> {
    let candidates: Vec<String> = std::env::var("QMAKE").map_or_else(
        |_| {
            [
                "qmake6",
                "/usr/lib/qt6/bin/qmake",
                "/usr/lib/qt6/bin/qmake6",
                "qmake",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        },
        |q| vec![q],
    );
    let query = |qmake: &str, key: &str| -> Option<String> {
        let out = Command::new(qmake).args(["-query", key]).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    let qmake = candidates
        .iter()
        .find(|q| query(q, "QT_VERSION").is_some_and(|v| v.starts_with("6.")))
        .ok_or_else(|| format!("pkg-config: {pkg_config_error}; and no Qt 6 qmake found"))?;
    let version = query(qmake, "QT_VERSION").unwrap_or_default();
    let minor: u32 = version
        .split('.')
        .nth(1)
        .and_then(|m| m.parse().ok())
        .unwrap_or(0);
    if minor < 2 {
        return Err(format!("Qt {version} is older than 6.2"));
    }
    let headers = PathBuf::from(query(qmake, "QT_INSTALL_HEADERS").ok_or("qmake gave no headers")?);
    let libs = PathBuf::from(query(qmake, "QT_INSTALL_LIBS").ok_or("qmake gave no libraries")?);
    let mut include_paths = vec![headers.clone()];
    let mut defines = Vec::new();
    for m in MODULES {
        include_paths.push(headers.join(format!("Qt{m}")));
        defines.push((format!("QT_{}_LIB", m.to_uppercase()), None));
        if !has_lib(&libs, m) {
            return Err(format!("libQt6{m} not in {}", libs.display()));
        }
        println!("cargo:rustc-link-lib=Qt6{m}");
    }
    println!("cargo:rustc-link-search=native={}", libs.display());
    println!("cargo:warning=Qt {version} found through {qmake} (no pkg-config files)");
    Ok(Qt {
        include_paths,
        defines,
    })
}

fn has_lib(dir: &Path, module: &str) -> bool {
    dir.join(format!("libQt6{module}.so")).exists()
}
