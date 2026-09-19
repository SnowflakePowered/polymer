pub use polymer_platform::ProcessId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct NativeWindow(pub isize);
