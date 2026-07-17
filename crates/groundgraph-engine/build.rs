//! Build script — compile the vendored SCIP protobuf schema to Rust types with
//! prost (issues.md #229).
//!
//! Replaces the runtime `scip` + `protobuf` crates. `protoc-bin-vendored`
//! ships a precompiled `protoc` for every release target, so the build needs no
//! system protoc and cross-compiles on macOS / Linux-musl / Windows. prost-build
//! shells out to that `protoc` to parse `proto/scip.proto`, then emits the
//! generated module into `OUT_DIR`.

fn main() {
    // Point prost-build (and the `protoc` crate it drives) at the vendored
    // binary so no system protoc is required.
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc binary");
    std::env::set_var("PROTOC", protoc);

    prost_build::Config::new()
        .compile_protos(&["proto/scip.proto"], &["proto"])
        .expect("compiling vendored SCIP schema (proto/scip.proto)");

    println!("cargo:rerun-if-changed=proto/scip.proto");
    println!("cargo:rerun-if-changed=build.rs");
}
