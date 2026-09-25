fn main() {
    // Keep the build script portable. Windows metadata is set through Cargo metadata
    // when a Windows resource tool is available in the release environment.
    println!("cargo:rerun-if-changed=build.rs");
}
