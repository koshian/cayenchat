#[cfg(windows)]
fn main() {
    println!("cargo:rerun-if-changed=resources/windows/cayenchat.rc");
    println!("cargo:rerun-if-changed=resources/windows/cayenchat.ico");
    embed_resource::compile("resources/windows/cayenchat.rc", embed_resource::NONE)
        .manifest_required()
        .expect("could not embed CayenChat icon");
}

#[cfg(not(windows))]
fn main() {}
