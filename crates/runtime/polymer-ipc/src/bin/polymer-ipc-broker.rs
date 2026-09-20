#[cfg(target_os = "macos")]
fn main() {
    let _broker = polymer_ipc::xpc::Broker::start();
    objc2_foundation::NSRunLoop::currentRunLoop().run();
}
#[cfg(not(target_os = "macos"))]
fn main() {
    panic!("The XPC endpoint broker is macOS-only");
}
