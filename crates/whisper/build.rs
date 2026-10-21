//! whisper.cpp v1.9.5 compiled against the ggml llama-cpp-sys-2 builds and links: two copies
//! of ggml in one program clash (duplicate symbols).

use std::path::PathBuf;

fn main() {
    let cmake_dir = PathBuf::from(std::env::var("DEP_LLAMA_GGML_CMAKE_DIR").expect("llama-cpp-sys-2 exports its ggml"));
    // <prefix>/lib/cmake → <prefix>/include
    let ggml_include = cmake_dir.parent().and_then(|p| p.parent()).expect("ggml prefix").join("include");
    let src = PathBuf::from("whisper.cpp");
    println!("cargo:rerun-if-changed=whisper.cpp");

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .file(src.join("src/whisper.cpp"))
        .file(src.join("src/aneforge/whisper-aneforge.cpp"))
        .include(src.join("include"))
        .include(src.join("src"))
        .include(&ggml_include)
        .define("WHISPER_VERSION", "\"1.9.5\"")
        .warnings(false);
    if build.get_compiler().is_like_msvc() {
        build.flag("/utf-8").flag("/EHsc");
    }
    build.compile("facteur_whisper");

    let bindings = bindgen::Builder::default()
        .header(src.join("include/whisper.h").to_string_lossy())
        .clang_arg(format!("-I{}", ggml_include.display()))
        .allowlist_function("whisper_.*")
        .allowlist_type("whisper_.*")
        .layout_tests(false)
        .generate()
        .expect("whisper.h bindings");
    bindings.write_to_file(PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("bindings.rs")).expect("bindings written");
}
