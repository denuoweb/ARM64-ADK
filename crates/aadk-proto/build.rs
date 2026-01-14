use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("failed to locate workspace root");
    let proto_root = workspace_root.join("proto");

    let protos = [
        proto_root.join("aadk/v1/common.proto"),
        proto_root.join("aadk/v1/errors.proto"),
        proto_root.join("aadk/v1/job.proto"),
        proto_root.join("aadk/v1/toolchain.proto"),
        proto_root.join("aadk/v1/project.proto"),
        proto_root.join("aadk/v1/build.proto"),
        proto_root.join("aadk/v1/target.proto"),
        proto_root.join("aadk/v1/observe.proto"),
        proto_root.join("aadk/v1/workflow.proto"),
    ];

    for p in &protos {
        println!("cargo:rerun-if-changed={}", p.display());
    }

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&protos, &[proto_root])
        .expect("failed to compile protos");
}
