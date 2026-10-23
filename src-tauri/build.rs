fn main() {
    // Vulkan's loader is only loaded once the local AI starts: a computer without it still opens
    // facteur, and its AI runs on the processor.
    if std::env::var_os("CARGO_FEATURE_VULKAN").is_some() && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:vulkan-1.dll");
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
    tauri_build::build()
}
