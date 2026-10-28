// SPDX-License-Identifier: GPL-3.0-only
use std::process::Command;

fn main() {
    emit_unix_desktop();
    emit_commit();
    #[cfg(windows)]
    windows_resources();
}

/// Sets `cfg(unix_desktop)` on the systems with GPUI's X11/Wayland backends and
/// the XDG desktop: Linux and FreeBSD (OpenBSD and NetBSD have no GPUI port).
/// This is the one place that names them, apart from the `[target.'cfg(...)']`
/// tables in `Cargo.toml`, which cannot use a custom cfg and repeat this list.
fn emit_unix_desktop() {
    println!("cargo::rustc-check-cfg=cfg(unix_desktop)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(os.as_str(), "linux" | "freebsd") {
        println!("cargo::rustc-cfg=unix_desktop");
    }
}

/// Hands the commit being built to the About window as `CAYENCHAT_COMMIT`.
/// `CAYENCHAT_COMMIT` in the environment wins (builds from a source archive
/// have no repository); with neither, nothing is set and the About window
/// shows no commit.
fn emit_commit() {
    println!("cargo:rerun-if-env-changed=CAYENCHAT_COMMIT");
    let git = |args: &[&str]| {
        let output = Command::new("git").args(args).output().ok()?;
        let text = String::from_utf8(output.stdout).ok()?;
        let text = text.trim();
        (output.status.success() && !text.is_empty()).then(|| text.to_owned())
    };
    // The checked-out ref moves without HEAD changing, so watch it as well.
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&["rev-parse", "--git-path", &reference])
    {
        println!("cargo:rerun-if-changed={path}");
    }
    if let Some(packed) = git(&["rev-parse", "--git-path", "packed-refs"]) {
        println!("cargo:rerun-if-changed={packed}");
    }
    let commit = std::env::var("CAYENCHAT_COMMIT")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| git(&["rev-parse", "--short=12", "HEAD"]));
    if let Some(commit) = commit.filter(|c| c.chars().all(|c| c.is_ascii_alphanumeric())) {
        println!("cargo:rustc-env=CAYENCHAT_COMMIT={commit}");
    }
}

#[cfg(windows)]
fn windows_resources() {
    use std::{env, fs, path::PathBuf};

    println!("cargo:rerun-if-changed=resources/windows/cayenchat.rc");
    println!("cargo:rerun-if-changed=resources/windows/cayenchat.ico");
    // The binary FILEVERSION fields are 16-bit; the string version keeps the full value.
    let number = |name: &str| {
        let value = env::var(name).expect(name);
        value.parse::<u64>().map_or(65535, |n| n.min(65535))
    };
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let numeric = format!(
        "{},{},{},0",
        number("CARGO_PKG_VERSION_MAJOR"),
        number("CARGO_PKG_VERSION_MINOR"),
        number("CARGO_PKG_VERSION_PATCH")
    );
    // The icon line comes from the checked-in rc; only VERSIONINFO is generated.
    let icon = fs::read_to_string("resources/windows/cayenchat.rc").expect("could not read rc");
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap().replace('\\', "/");
    let icon = icon.replace("\"resources/", &format!("\"{manifest_dir}/resources/"));
    let rc = format!(
        r#"{icon}
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
{{
    BLOCK "StringFileInfo"
    {{
        BLOCK "040904B0"
        {{
            VALUE "CompanyName", "CayenChat"
            VALUE "FileDescription", "CayenChat IRC client"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "cayenchat"
            VALUE "OriginalFilename", "cayenchat.exe"
            VALUE "ProductName", "CayenChat"
            VALUE "ProductVersion", "{version}"
        }}
    }}
    BLOCK "VarFileInfo"
    {{
        VALUE "Translation", 0x409, 1200
    }}
}}
"#
    );
    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("cayenchat.rc");
    fs::write(&path, rc).expect("could not write rc");
    embed_resource::compile(path, embed_resource::NONE)
        .manifest_required()
        .expect("could not embed CayenChat icon and version");
}
