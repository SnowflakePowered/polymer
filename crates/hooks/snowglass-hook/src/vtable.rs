#[macro_export]
macro_rules! vtable {
    ($($vtbl:ty),+ $(,)?) => {
        pub(crate) trait Vtable: 'static {
            const SLOTS: usize;

            fn slots() -> usize {
                Self::SLOTS
            }
        }

        $(
            impl Vtable for $vtbl {
                const SLOTS: usize =
                    ::core::mem::size_of::<$vtbl>() / ::core::mem::size_of::<usize>();
            }
        )+
    };
}
