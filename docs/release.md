# Release builds (Linux aarch64)

AADK services and the GTK UI are only supported on Linux aarch64. This guide
captures the release build steps for GitHub assets.

## Build all binaries
```bash
cargo build --release --workspace
ls -1 target/release/aadk-*
```

## Package a release archive
```bash
VERSION=0.1.0
OUT=dist/aadk-${VERSION}-linux-aarch64
mkdir -p "${OUT}"
cp target/release/aadk-{core,workflow,toolchain,project,build,targets,observe,ui,cli} "${OUT}/"
cp scripts/dev/run-all.sh README.md LICENSE "${OUT}/"
tar -C dist -czf "aadk-${VERSION}-linux-aarch64.tar.gz" "aadk-${VERSION}-linux-aarch64"
sha256sum "aadk-${VERSION}-linux-aarch64.tar.gz" > "aadk-${VERSION}-linux-aarch64.tar.gz.sha256"
```

## Scripted release build
```bash
scripts/release/build.sh
```

Override the version:
```bash
VERSION=0.1.0 scripts/release/build.sh
```
