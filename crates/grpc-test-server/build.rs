//! Compiles testing.proto with protox, so no `protoc` binary is needed here
//! or in CI. The descriptor set is also written out so the server can serve
//! reflection from it.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protos = ["protos/testing.proto"];
    let includes = ["protos"];

    println!("cargo:rerun-if-changed=protos/testing.proto");

    let descriptors = protox::compile(protos, includes)?;

    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);
    let descriptor_path = out_dir.join("testing_descriptor.bin");
    std::fs::write(
        &descriptor_path,
        prost::Message::encode_to_vec(&descriptors),
    )?;

    tonic_prost_build::configure()
        .build_client(false)
        .build_server(true)
        .file_descriptor_set_path(&descriptor_path)
        .compile_fds(descriptors)?;

    Ok(())
}
