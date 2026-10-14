//! Build script: on Linux, compile `src/plugin.cpp` against Qt 6 Widgets (found
//! with pkg-config) and `ot-shell-abi`'s header, and link Qt. Elsewhere there is
//! nothing to build; the crate is empty.
//!
//! No `moc` step: the C++ uses only functor connections and overridden event
//! handlers, which need no meta-object code.

use std::path::PathBuf;

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

    let qt = pkg_config::Config::new()
        .atleast_version("6.2")
        .probe("Qt6Widgets")
        .unwrap_or_else(|e| {
            panic!(
                "Qt 6 Widgets not found ({e}). Install its development files \
                 (Debian and Ubuntu: qt6-base-dev), or build without this crate: \
                 cargo build --workspace --exclude ot-shell-qt"
            )
        });

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .file("src/plugin.cpp")
        .include(&abi)
        .std("c++17")
        .flag("-fPIC")
        // Qt is built with -reduce-relocations; code using it must be
        // position-independent, and its symbols are not exported from here.
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
    // pkg-config already told cargo to link Qt6Widgets, Qt6Gui and Qt6Core.
}
