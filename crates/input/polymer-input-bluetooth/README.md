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
orientation quaternion is claimed. The Bluetooth session queries Feature 0x01
and EEPROM page 0xA0 to select the hardware-trim, plain-zero, or SIXAXIS path.
When those queries succeed, the decoder uses the per-axis factory zero/span
and the session's current gyro zero. Hardware trim is preserved in every LED
and rumble output; SIXAXIS uses the otherwise-unused large-motor slot.

Hardware-trim sessions re-measure the resting gyro after four consecutive
quiet 16-sample blocks. They wait 32 samples initially and two after an
acknowledged trim change, then re-measure rather than assuming the trim has
taken effect. The byte is bounded to 0..255. This is a conservative rest-only
tracker, not a byte-for-byte Sony tracker: it omits the moving long-term
average fallback and never wraps the trim byte. Yaw is suppressed while the
initial zero or a new hardware trim is settling. External protocol ownership
stops the tracker; reconnect/reacquisition reloads factory data.

Unsupported or malformed calibration feature responses use the nominal
conversion and log the reason. A timed-out control transaction requires
reconnection, since HID replies have no transaction IDs. Bluetooth Feature 0x01 and 0xEF reads were verified on controller
04:98:F3:26:42:A5: hardware path, factory trim 0x7B, acceleration zeros
512/514/510 and spans 113/112/112. Its raw gyro changed from about 5 to
520 after trim activation, and the rest tracker converged near 520. The user
confirmed yaw responds in both directions and returns near zero at rest. Bluetooth
feature replies begin with the report ID (01 01 / EF EF); USB replies use
00 01 / 00 EF at the same offsets.
Existing motion calibration can further correct device-specific bias and scale. Battery categories become coarse percentages;
charging is reported independently and unknown charge remains unknown.

Protocol references:

- [DsHidMini native report layout](https://github.com/nefarius/DsHidMini/blob/v3.3.1187.0/include/DsHidMini/Ds3Types.h)
- [SDL native PS3 acceleration and button mapping](https://github.com/libsdl-org/SDL/blob/main/src/joystick/hidapi/SDL_hidapi_ps3.c)
- [DsHidMini calibration protocol research](https://github.com/nefarius/DsHidMini/blob/master/docs/MOTION.md)
- [RPCS3 DS3 gyro conversion](https://github.com/RPCS3/rpcs3/blob/master/rpcs3/Emu/Io/PadHandler.cpp)

Regression tests cover native report decoding through the backend, pressure
release/zero handling, byte order and motion units, invalid reports, battery
states, factory-page parsing, trim preservation, rest detection, remapping,
and IPC serialization. Other DS3 revisions and original SIXAXIS hardware have
not been tested.
