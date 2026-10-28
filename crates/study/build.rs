fn main() {
    println!("cargo:rerun-if-changed=study.rc");
    println!("cargo:rerun-if-changed=../../assets/study.ico");
    // The exe icon (Explorer, taskbar pins). Skipped quietly if no resource compiler is installed.
    let _ = embed_resource::compile("study.rc", embed_resource::NONE).manifest_optional();
}
