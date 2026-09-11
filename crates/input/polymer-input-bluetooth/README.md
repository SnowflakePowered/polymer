# Bluetooth input decoding

`decoder::for_profile` selects the normalized decoder after the Bluetooth driver
has activated a controller. Decoders consume HID reports including the report ID,
without a HIDP transport header.

## DualShock 3

`sony.dualshock3` accepts native 49-byte report `0x01`, rejecting incomplete
reports and the `0xff` invalid-data marker. Face buttons use position mapping:
Cross → A, Circle → B, Square → X, Triangle → Y. Sticks are centered at 128,
with negative Y pointing up.

The twelve native pressure bytes are preserved: ten face/shoulder/D-pad values
in `GamepadReport::pressure` (0–255), and L2/R2 in the existing 16-bit trigger
fields (byte × 257). Pressure does not replace the independent digital bits.
Released digital contacts clear residual pressure bytes, including L2/R2.
`button_pressure` falls back to 0/255 for digital-only inputs. Pressure survives
hub composition, button remapping, and IPC to the hook's input state; mapping a
pressure button to a trigger also uses its analog value. Standard SDL gamepad
button APIs remain digital.

Motion uses native big-endian ADC words, not the transformed Windows SXS
format. Acceleration follows SDL's PS3 axis convention and nominal conversion
(113 counts/g, center 511) into m/s². The single yaw gyro is published on Y in
radians/s (center 512, 123 counts per 90 degrees/s); X/Z remain zero and no
orientation quaternion is claimed. Existing motion calibration can correct
device-specific bias and scale. Battery categories become coarse percentages;
charging is reported independently and unknown charge remains unknown.

Protocol references:

- [DsHidMini native report layout](https://github.com/nefarius/DsHidMini/blob/v3.3.1187.0/include/DsHidMini/Ds3Types.h)
- [SDL native PS3 acceleration and button mapping](https://github.com/libsdl-org/SDL/blob/main/src/joystick/hidapi/SDL_hidapi_ps3.c)
- [RPCS3 DS3 gyro conversion](https://github.com/RPCS3/rpcs3/blob/master/rpcs3/Emu/Io/PadHandler.cpp)

Regression tests cover native report decoding through the backend, pressure
release/zero handling, byte order and motion units, invalid reports, battery
states, remapping, and IPC serialization. Live wireless motion still needs
verification on hardware.
