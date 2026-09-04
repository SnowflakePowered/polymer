use std::sync::Arc;

use super::{
    ControllerDevice,
    generic::GenericHidController,
    nintendo::{
        switch::{
            JoyConLeft, JoyConRight, NsoGenesisController, NsoN64Controller, NsoNesController,
            NsoSnesController, SwitchProController,
        },
        switch2::{
            JoyCon2Left, JoyCon2Right, NsoGameCubeController, Switch2ProController,
            UnknownSwitch2Controller,
        },
        wiimote::{UnknownWiiRemote, WiiRemote, WiiRemotePlus},
    },
    sony::{DualSense, DualShock3, DualShock4, UnknownSonyController},
};

pub(super) fn controllers() -> Vec<Arc<dyn ControllerDevice>> {
    vec![
        Arc::new(SwitchProController::default()),
        Arc::new(JoyConLeft::default()),
        Arc::new(JoyConRight::default()),
        Arc::new(NsoNesController::default()),
        Arc::new(NsoSnesController::default()),
        Arc::new(NsoN64Controller::default()),
        Arc::new(NsoGenesisController::default()),
        Arc::new(WiiRemote::default()),
        Arc::new(WiiRemotePlus::default()),
        Arc::new(UnknownWiiRemote::default()),
        Arc::new(JoyCon2Left::default()),
        Arc::new(JoyCon2Right::default()),
        Arc::new(Switch2ProController::default()),
        Arc::new(NsoGameCubeController::default()),
        Arc::new(UnknownSwitch2Controller::default()),
        Arc::new(DualShock3),
        Arc::new(DualShock4),
        Arc::new(DualSense),
        Arc::new(UnknownSonyController),
        Arc::new(GenericHidController),
    ]
}
