// Emscripten link settings for the web viewer: an ES module factory (`createStudyModule`) that
// draws through a WebGL2 context the page creates (see js/study-viewer.js).
fn main() {
    println!("cargo:rerun-if-changed=src/sjlj.cpp");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("emscripten") {
        return;
    }
    for arg in [
        "-sMODULARIZE=1",
        "-sEXPORT_ES6=1",
        "-sEXPORT_NAME=createStudyModule",
        "-sENVIRONMENT=web",
        "-sMIN_WEBGL_VERSION=2",
        "-sMAX_WEBGL_VERSION=2",
        "-sALLOW_MEMORY_GROWTH=1",
        "-sMAXIMUM_MEMORY=4GB",
        "-sSTACK_SIZE=4MB",
        "-sERROR_ON_UNDEFINED_SYMBOLS=0",
        "-sEXPORTED_RUNTIME_METHODS=GL,UTF8ToString,HEAPU8",
    ] {
        println!("cargo:rustc-link-arg={arg}");
    }
    // em++ compiles source files on its link line: the setjmp/longjmp shim the prebuilt Skia needs.
    let shim = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("src/sjlj.cpp");
    println!("cargo:rustc-link-arg={}", shim.display());
}
