//! Nintendo-specific connection-time protocols.

macro_rules! controller_model {
    ($controller:ident = $device:ident<$model:ident> impl $model_trait:ident {
        $($implementation:item)*
    }) => {
        #[doc(hidden)]
        pub struct $model;

        impl $model_trait for $model {
            $($implementation)*
        }

        pub type $controller = $device<$model>;
    };
}

const NINTENDO_VENDOR_ID: u16 = 0x057e;

pub mod switch;
pub mod switch2;
pub mod wiimote;
