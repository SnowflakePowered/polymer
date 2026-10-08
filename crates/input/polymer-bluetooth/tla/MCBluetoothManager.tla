------------------------- MODULE MCBluetoothManager -------------------------
EXTENDS BluetoothManager, TLC

MCAddrs == {"pad", "wiimote", "joycon"}
MCRadio == ("pad" :> "bredr") @@ ("wiimote" :> "bredr") @@ ("joycon" :> "ble")
=============================================================================
