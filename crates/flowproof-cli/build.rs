//! Windows gives a program's main thread a 1 MB stack, where Linux and macOS
//! give 8 MB. In a debug build the command dispatch and the record path keep
//! large frames, and `record` overflowed that 1 MB on Windows once the
//! command list grew by one (`author-from-video`). Link the binary with the
//! 8 MB the other platforms already run it with.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("msvc") => println!("cargo:rustc-link-arg-bins=/STACK:8388608"),
        Ok("gnu") => println!("cargo:rustc-link-arg-bins=-Wl,--stack,8388608"),
        _ => {}
    }
}
