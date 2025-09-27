use std::env;

fn main() {
    // Tell Cargo to rerun this build script if source files change
    println!("cargo:rerun-if-changed=src/");
    println!("cargo:rerun-if-changed=plugin.toml");

    // Set up linker flags for undefined symbols (resolved at runtime from host)
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-arg=-undefined");
        println!("cargo:rustc-link-arg=dynamic_lookup");
    } else if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-arg=-Wl,--allow-shlib-undefined");
    }

    // The plugin will be built to its own target directory
    // No copying needed - the host will load from crates/plugins/diagnostics/target/debug/
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    println!("cargo:warning=Plugin building as {} profile", profile);
}