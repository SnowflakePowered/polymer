import FSKit

// FSKit's AppExtension lifecycle entry point is a Swift-only protocol.
// The filesystem and every volume operation are implemented in Rust/objc2.
@main
struct PolymerExtension: UnaryFileSystemExtension {
    let fileSystem = PolymerCreateFileSystem()
}
