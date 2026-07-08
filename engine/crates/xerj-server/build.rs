//! Build script: compile `proto/xerj.proto` into Rust gRPC stubs.
//!
//! We deliberately do NOT depend on a host `protoc` binary. `protox` is a
//! pure-Rust protobuf compiler that parses the `.proto` file and produces a
//! `FileDescriptorSet`, which `tonic-build` then turns into the prost message
//! types plus the `XerjSearch` client/server stubs via `compile_fds`. This
//! keeps the build hermetic and portable across the whole cross-compile
//! matrix (musl / aarch64 / windows / macos) where `protoc` is not
//! guaranteed to be installed.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("proto");
    let proto_file = proto_root.join("xerj.proto");

    println!("cargo:rerun-if-changed={}", proto_file.display());
    println!("cargo:rerun-if-changed=build.rs");

    // Pure-Rust parse → FileDescriptorSet (no `protoc` process spawned).
    let file_descriptors = protox::compile([&proto_file], [&proto_root])?;

    // FileDescriptorSet → Rust (messages + XerjSearch client & server).
    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_fds(file_descriptors)?;

    Ok(())
}
