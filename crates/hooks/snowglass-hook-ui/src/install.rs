pub(crate) unsafe fn install() -> i32 {
    // The UI library is injected after GUI runtimes and worker threads are
    // active. Its modal interceptors use inline code patches, which cannot be
    // installed safely while another thread may execute the patched entry
    // points. Keep injection compatible, but leave modal forwarding disabled
    // until installation can be moved before the target process is resumed.
    0
}
