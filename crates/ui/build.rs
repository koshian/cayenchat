#[cfg(windows)]
fn main() {
    use std::{env, fs, path::PathBuf};

    println!("cargo:rerun-if-changed=resources/windows/cayenchat.rc");
    println!("cargo:rerun-if-changed=resources/windows/cayenchat.ico");
    let number = |name: &str| env::var(name).expect(name);
    let version = number("CARGO_PKG_VERSION");
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

#[cfg(not(windows))]
fn main() {}
