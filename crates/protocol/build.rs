fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/herdcore.proto");
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .build_transport(false)
        .compile_protos(&["proto/herdcore.proto"], &["proto"])?;
    Ok(())
}
