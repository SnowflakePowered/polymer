# NSO GameCube Controller Native Report `0x0A`

This document records the current reverse-engineering state of the packed motion
payload emitted by the Nintendo Switch 2 NSO GameCube controller. It separates
observations that are confirmed by captures from interpretations that remain
hypotheses.

## Capture corpus

The findings below were derived from these captures:

- `%LOCALAPPDATA%\Polymer\captures\switch2-gamecube-motion-1787817891826-3C-A9-AB-93-02-36.jsonl`
  - Native report `0x0A`
  - 43,365 notifications
  - Labeled stationary orientations, slow positive and negative rotations on all
    three axes, fast flicks, isolated taps, and a long stationary interval
- `%LOCALAPPDATA%\Polymer\captures\switch2-gamecube-motion-1787819268289-3C-A9-AB-93-02-36.jsonl`
  - Common report `0x05`
  - 13,867 notifications
  - The same labeled sequence, used as the uncompressed reference
- `%LOCALAPPDATA%\Polymer\captures\switch2-gamecube-motion-1787840127427-3C-A9-AB-93-02-36.jsonl`
  - Automatic native/common report experiment
  - 7,249 native and 6,240 common notifications
  - Concurrent GATT subscriptions followed by acknowledged 250 ms alternation
  - Modes 0, 1, and 2 exercised repeatedly during the three flick trials
- `%LOCALAPPDATA%\Polymer\captures\switch2-gamecube-motion-1787844007353-3C-A9-AB-93-02-36.jsonl`
  - Guided quaternion and timing experiment
  - 24,970 native notifications; the complete run begins at `t_ns =
    42,761,290,600` after one aborted attempt
  - 25 stationary holds and isolated positive/negative rotations through full
    turns about all three physical axes
  - 21,879 usable normal-class packets in the completed run
- `%LOCALAPPDATA%\Polymer\captures\switch2-gamecube-motion-1787883791577-3C-A9-AB-93-02-36.jsonl`
  - Guided lane-isolation experiment
  - 196,941 decoded native motion packets
  - Repeated stationary, rotation, translation, and closed-loop actions
- `%LOCALAPPDATA%\Polymer\captures\switch2-gamecube-motion-1787933425359-3C-A9-AB-93-02-36.jsonl`
  - Guided class-`0xD` experiment and long stationary collection
  - 1,974,875 native notifications
- `%LOCALAPPDATA%\Polymer\captures\switch2-gamecube-motion-1788012704011-3C-A9-AB-93-02-36.jsonl`
  - Focused class-`0xF` timing experiment
  - 19,049 native notifications and 186 class-`0xF` packets
  - Stationary, steady pitch/roll/yaw, mixed-flick, isolated-tap, and charging
    actions

For Bluetooth notifications, the HID report ID is omitted. In report `0x0A`, byte
`0x0E` is the motion-data length and byte `0x0F` is the start of the motion data.

Observed motion lengths in the native capture:

| Length | Count | Notes |
| ---: | ---: | --- |
| 40 | 43,003 | Normal full packet |
| 30 | 199 | Short/transitional packet; layout not decoded |
| 4 | 163 | Header only |

## Confirmed 40-byte structure

The 40-byte motion payload consists of a four-byte header followed by a 36-byte,
LSB-first packed bitstream:

```text
motion[0..4]   header
motion[4..40]  packed body (288 bits)
```

All bit offsets below are relative to `motion[4]`, not the start of the complete
BLE notification.

### Header

Interpreting the first four bytes as a little-endian integer:

```text
bits  0..11  timestamp modulo 4096
bits 12..23  elapsed timestamp ticks since the preceding full packet
bits 24..27  packet/timing class
bits 28..31  observed as zero
```

Normal 40-byte packets usually have an elapsed value around 12 ticks. The
timestamp source runs at 800 Hz. The timestamp delta between consecutive normal
packets equals the elapsed field. Timing class `0xE` identifies the normal layout
documented below. Across the complete corpus there are 22,052 class-`0xF` and
167 class-`0xD` full packets. They use separate layouts and must not be passed
through the class-`0xE` field map.

### Normal-class terminal keyframe

In timing class `0xE`, the last 100 bits contain a complete IMU keyframe. These
locations remain fixed when bits 0-1 change mode.

| Body bits | Width | Type | Meaning |
| ---: | ---: | --- | --- |
| 188 | 14 | signed | Gyroscope X |
| 202 | 14 | signed | Gyroscope Y |
| 216 | 14 | signed | Gyroscope Z |
| 230 | 14 | signed | Accelerometer X |
| 244 | 14 | signed | Accelerometer Y |
| 258 | 14 | signed | Accelerometer Z |
| 272 | 16 | signed | Higher-resolution temperature value |

The 14-bit fields use two's-complement sign extension. Pseudocode:

```text
gyro[i] = sign_extend(read_bits_le(body, 188 + i * 14, 14), 14)
accel[i] = sign_extend(read_bits_le(body, 230 + i * 14, 14), 14)
temperature_q6 = sign_extend(read_bits_le(body, 272, 16), 16)
```

After applying the adaptive-range multiplier documented below, the accelerometer
and gyroscope values use the same scale as common report `0x05`:

```text
acceleration_g = accel / 4096
angular_velocity_dps = gyro / 16.4
common_temperature_value = temperature_q6 / 64
temperature_celsius = temperature_q6 / 128 + 25
```

The Celsius conversion is the ICM-42670-P's documented 16-bit temperature
conversion. Equivalently, report `0x05` carries the eight-bit FIFO temperature
unit whose conversion is `value / 2 + 25`.

The fixed keyframe was identified independently of the labels and then verified
against the common capture. During the final stationary phase, for example, both
formats reported approximately:

```text
gyro  = [9, -13, -14]
accel = [-17, -127, 4055]
```

The expected single-axis response also appears at these exact offsets during all
six positive/negative rotation phases.

## Confirmed projective quaternion endpoint

The first 68 body bits contain an adaptive-range selector and a projectively
encoded quaternion:

| Body bits | Width | Meaning |
| ---: | ---: | --- |
| 0 | 1 | Accelerometer range flag |
| 1 | 1 | Gyroscope range flag |
| 2 | 2 | Index of the largest quaternion component, in `w, x, y, z` order |
| 4 | 1 | Reserved/padding; zero in every captured normal packet |
| 5 | 21 | First biased projective coordinate |
| 26 | 21 | Second biased projective coordinate |
| 47 | 21 | Third biased projective coordinate |

This is not a smallest-three unit-sphere encoding. Each stored coordinate is the
ratio of another quaternion component to the selected largest component. Decode
it as follows:

```text
r[i] = (raw21[i] - 0x100000) / 0x100000

q = [0, 0, 0, 0]
q[face] = 1
q[(face + 1) & 3] = r[0]
q[(face + 2) & 3] = r[1]
q[(face + 3) & 3] = r[2]
q = normalize(q)
```

The coordinate placement is cyclic after the selected face, not ascending order
with one component omitted. Expanded explicitly:

| Face | Unnormalized `q = [w, x, y, z]` |
| ---: | --- |
| 0 | `[1, r0, r1, r2]` |
| 1 | `[r2, 1, r0, r1]` |
| 2 | `[r1, r2, 1, r0]` |
| 3 | `[r0, r1, r2, 1]` |

As usual, `q` and `-q` represent the same rotation. Hemisphere-align consecutive
outputs by negating the new quaternion when its dot product with the preceding
one is negative.

Evidence that closes the earlier ambiguity:

- The selected face is the largest-magnitude decoded component in all 21,879
  normal packets from the completed guided run.
- The cyclic mapping remains continuous through all 29 observed face changes:
  median step `0.93` degrees, 90th percentile `2.74` degrees, maximum `4.81`
  degrees while the controller was moving. The previous ascending mapping made
  those boundaries jump by roughly 90-180 degrees.
- The local/body quaternion delta `conjugate(q[n-1]) * q[n]` matches the raw
  gyroscope vector component-for-component. Across the isolated transition
  phases, per-axis correlation is `0.996-0.997` and fitted scale is within `0.6%`
  of the documented `16.4 counts/(degree/second)` scale.
- Direct smallest-three reconstruction produced an invalid square root in about
  `3.7%` of the original native capture, concentrated in roll poses. Projective
  reconstruction has no invalid domain.

The component order is therefore `w, x, y, z`, and the quaternion's right/local
differential uses the same body-axis order and signs as the report's gyroscope.
The controller's choice of initial world reference is still arbitrary; consumers
should establish their own reference or recenter.

### Physical axes and viewer basis

The decoded quaternion and gyroscope use the same native sensor axes. Repeated
live manipulation with the relative-orientation viewer established this mapping:

| Native sensor component | Physical/model rotation |
| --- | --- |
| X | Pitch |
| Y | Roll |
| Z | Yaw |

The bttest model therefore consumes the relative native quaternion without a
component permutation.

The viewer computes the relative orientation before rendering:

```text
q_relative_native = conjugate(q_reference) * q_current
q_relative_model  = q_relative_native
```

The multiplication order is not interchangeable. The controller's local/body
differential is `conjugate(q_previous) * q_current`, as independently established
by its correlation with the native gyroscope. Using
`q_current * conjugate(q_reference)` expresses the relative rotation in the
controller's arbitrary startup world frame; in live tests that left only roll
approximately correct and mixed pitch with yaw.

Two attempted component permutations, `[w, z, x, y]` and `[w, x, z, y]`, were
also rejected by live tests. Do not reintroduce either mapping to compensate for
the wrong multiplication order.

The guided capture provides a direct physical-axis check. RMS terminal gyroscope
counts across the marked transitions were:

| Marked motion | Native X | Native Y | Native Z |
| --- | ---: | ---: | ---: |
| Pitch | 517.5 | 240.1 | 158.3 |
| Roll | 247.2 | 757.1 | 204.8 |
| Yaw | 9.8 | 54.2 | 501.7 |

The secondary energy comes from hand movement while entering and leaving the
guided positions; the dominant component establishes the X/Y/Z assignment.

### Quaternion pipeline delay

The quaternion is 12 sensor ticks older than the packet terminal. This is
approximately one report behind the raw samples carried alongside it, but it is
not tied to the preceding report boundary. In an elapsed-11, 12, or 13 packet,
its position relative to the preceding terminal is tick `-1`, `0`, or `+1`
respectively. A four-keyframe interpolation independently recovers approximately
`-1.3`, `0.0`, and `+1.3` ticks for those three elapsed values.

`q[n-1] -> q[n]` therefore correlates with the terminal gyroscope around packet
`n-1`, not packet `n`. Using the preceding packet gives correlation
`0.996-0.997` on every axis; using the current packet drops it to approximately
`0.966-0.984` during the guided motions. Timestamp the quaternion in packet `n`
at `packet_terminal - 12 / 800 seconds` rather than copying a host timestamp
from the preceding notification.

## Confirmed IMU history area

For timing class `0xE`, bits 68-187 have one fixed layout in all four values of
bits 0-1. They contain absolute historical IMU samples, not predictive residuals.

| Body bits | Width | Type | Meaning |
| ---: | ---: | --- | --- |
| 68 | 14 | signed | Earlier accelerometer X |
| 82 | 14 | signed | Earlier accelerometer Y |
| 96 | 14 | signed | Earlier accelerometer Z |
| 110 | 13 | signed, `<< 1` | Earlier gyroscope X |
| 123 | 13 | signed, `<< 1` | Earlier gyroscope Y |
| 136 | 13 | signed, `<< 1` | Earlier gyroscope Z |
| 149 | 13 | signed, `<< 1` | Middle accelerometer X |
| 162 | 13 | signed, `<< 1` | Middle accelerometer Y |
| 175 | 13 | signed, `<< 1` | Middle accelerometer Z |

The 13-bit quantities discard the raw sensor value's least-significant bit:

```text
gyro_earlier[i] = sign_extend(read_bits_le(body, 110 + i * 13, 13), 13) << 1
accel_middle[i] = sign_extend(read_bits_le(body, 149 + i * 13, 13), 13) << 1
```

### Adaptive ranges

Body bits 0 and 1 independently select the packed accelerometer and gyroscope
ranges. A clear flag means that the corresponding signed fields were divided by
four to gain range and must be multiplied by four during decoding:

| Bits 1:0 | Accelerometer multiplier | Gyroscope multiplier |
| ---: | ---: | ---: |
| `00` | 4 | 4 |
| `01` | 1 | 4 |
| `10` | 4 | 1 |
| `11` | 1 | 1 |

Apply the accelerometer multiplier to all three accelerometer groups and the
gyroscope multiplier to both gyroscope groups, after sign extension and the
13-bit fields' existing `<< 1` reconstruction. In physical units, the clear-flag
modes extend the packed range from approximately +/-2 g to +/-8 g and from
approximately +/-500 degrees/second to +/-2000 degrees/second.

The multiplier is not inferred merely from those nominal ranges. At transitions
of bit 0 in the original native capture, a factor-of-four acceleration decode
reduced the median within-packet second-difference error to 117 raw common-report
counts, versus 321 unscaled, 282 at a factor of three, and 357 at a factor of
five. The equivalent gyro test selected four with a 170-count median error,
versus 1,313 unscaled, 460 at three, and 533 at five. Independently, quaternion
angular velocity requires exactly the same factor-of-four gyro correction.

Together with the terminal keyframe, each normal packet therefore provides three
accelerometer samples and two gyroscope samples:

```text
accelerometer: earlier @ 68, middle @ 149, terminal @ 230
gyroscope:     earlier @ 110, terminal @ 188
```

Chronology is confirmed by continuity and second differences. Across consecutive
normal packets, the sequence `previous terminal -> earlier -> middle -> terminal`
is smooth for acceleration, and `previous terminal -> earlier -> terminal` is
smooth for angular velocity.

The guided capture also resolves the sample positions to the 800 Hz header clock.
Let `E` be the current packet's elapsed field and let tick zero be the preceding
normal packet's terminal sample:

```text
accelerometer earlier   round(E / 3)
accelerometer middle    ceil(2 * E / 3)
gyroscope earlier       ceil(2 * E / 3)
accelerometer terminal  E
gyroscope terminal      E
```

For every normal elapsed value observed in the completed run, this becomes:

| Elapsed `E` | Accel earlier | Accel middle / gyro earlier | Terminal |
| ---: | ---: | ---: | ---: |
| 11 | 4 | 8 | 11 |
| 12 | 4 | 8 | 12 |
| 13 | 4 | 9 | 13 |

Interpolating the surrounding terminal keyframes independently placed the samples
at `3.85/7.70`, `3.96/7.68`, and `4.03/8.97` ticks for elapsed values 11, 12, and
13 respectively. The integer schedule above is the nearest 800 Hz schedule and
is materially better than assigning generic host-time spacing. Elapsed values
outside 11-13 have not been exercised in an uninterrupted normal stream.

The alleged selector at body bit 128 is disproven. It is bit 5 of the earlier
Gyroscope Y field at bits 123-135. Its four-value distribution merely reflected
sensor data.

The layout above was verified separately for normal packets whose first two bits
were 0, 1, 2, and 3. The alternation capture contained 182, 119, 114, and 6,256
such packets respectively. The values do not rearrange the fields; they select
the independent range multipliers documented above.

### Important rejected interpretation

An initial fit suggested this width table for the fields beginning at bit 68:

| Mode | Absolute width | Residual width | Quantization shift |
| ---: | ---: | ---: | ---: |
| 0 | 11 | 8 | 3 |
| 1 | 12 | 7 | 2 |
| 2 | 13 | 6 | 1 |
| 3 | 14 | 5 | 0 |

It is elegant but incorrect. The apparent fit was caused by treating the two
signed 13-bit absolute fields as residuals and by including non-normal timing
classes. Do not implement this table.

## Short packets

### 30-byte class-`0xC` packet

This is a shared Switch 2 native-motion packet, not an NSO GameCube-specific
format. The GameCube controller supplied the capture corpus used to decode it,
while Joy-Con 2 and Pro Controller 2 reach the same decoder through their own
native-report envelopes.

The 30-byte packet is a real intermediate sensor packet, not padding and not the
40-byte layout with ten bytes removed. All 849 examples in the completed guided
GameCube capture use timing class `0xC`. Their elapsed count is normally one or
two ticks (with a single observed value of three), and the next normal packet's elapsed count is
measured from this packet's sensor timestamp rather than from the preceding
normal packet. Every captured 30-byte packet is immediately surrounded by
normal 40-byte packets.

After the common four-byte header, the 26-byte body has this decoded layout:

| Body bit | Width | Interpretation |
| ---: | ---: | --- |
| 0 | 2 | Index of the largest quaternion component, in `w, x, y, z` order |
| 2 | 6 | Reserved/padding; zero in the corpus |
| 8 | 24 | Folded first projective coordinate |
| 32 | 2 | Four-way range selector for the first coordinate |
| 34 | 6 | Reserved/padding; zero in the corpus |
| 40 | 24 | Folded second projective coordinate |
| 64 | 1 | Two-way range selector for the second coordinate |
| 65 | 7 | Reserved/padding; zero in the corpus |
| 72 | 24 | Third projective coordinate |
| 96 | 32 | Accelerometer X, signed Q16.16 in normal raw-count units |
| 128 | 32 | Accelerometer Y, signed Q16.16 in normal raw-count units |
| 160 | 32 | Accelerometer Z, signed Q16.16 in normal raw-count units |
| 192 | 16 | Temperature, signed Q8.7 relative to 25 degrees Celsius |

Let `u0`, `u1`, and `u2` be the three unsigned little-endian 24-bit
payloads, and let `s0` and `s1` be the selectors at bits 32 and 64. Recover the
projective coordinates with:

```text
c0 = (u0 - 0x800000) / 0x800000
c1 = (u1 - 0x800000) / 0x800000
c2 = (u2 - 0x800000) / 0x800000

r0 = (c0 + (2 * s0 - 3)) / 4
r1 = (c1 + (2 * s1 - 1)) / 2
r2 = c2
```

Reconstruct and normalize the quaternion with the same cyclic face mapping as
the normal 40-byte packet. The first coordinate is effectively signed Q1.25,
the second Q1.24, and the third Q1.23, compared with Q1.20 for all three normal
coordinates.

This mapping is exact across all 849 class-`0xC` packets in the completed guided
capture. The face field matches the surrounding normal quaternion in every
packet. The first selector maps `0, 1, 2, 3` to offsets `-3, -1, +1, +3` with no
exceptions, while the second selector maps `0, 1` to `-1, +1`; its reserved high
bit never became set. After unfolding, the median angular difference from the
following normal keyframe is `0.0066` degrees.

Decode acceleration component `a` and temperature component `t` as:

```text
raw_accel_counts = sign_extend_32(a) / 65536
accel_m_s2       = raw_accel_counts / 4096 * 9.80665
temperature_C    = sign_extend_16(t) / 128 + 25
```

The Q16.16 accelerometer values correlate with an interpolation of the adjacent
normal-packet acceleration samples at greater than `0.99994` on every axis. The
best direct fits have slopes within `0.11%` of one. Reanalysis of 8,379 usable
`E -> C -> E` windows initially placed the dynamic subset's effective point
roughly 1.5-1.8 ticks after the immediately preceding normal packet's terminal
sample, but only 165 of those windows contained strong excitation.

A frequency-controlled follow-up resolves the placement at **1.5 sensor ticks
after the preceding normal terminal**. Gain-and-offset-calibrated cubic fits put
X slow at 1.463 ticks, X fast at 1.470, isolated X impulses at 1.486, and Y
medium at 1.539. The fast-X action supplied the strongest signal: its ten
contexts had a median 1,406-count span and a fitted slope of 1.0019. The packet
header advanced by either one or two ticks in the wider corpus, but the vector's
effective point remained fixed at 1.5 ticks rather than following the header
timestamp. Treat it as a filtered/resampled acceleration sample at that
fractional position, not as a new ADC sample.

bttest's dedicated **class-C acceleration filter capture** separates frequency
from physical axis. It records a stationary baseline, slow/medium/fast
fixed-orientation translations on X/Y/Z, and isolated impulses on all three
axes. Each accepted marker is a complete `E -> C -> E` window containing the
decoded previous terminal, all three following normal acceleration samples, the
Q16.16 class-C vector, sensor-clock sample positions, and host-time gaps. The
stationary action targets 12 contexts and each dynamic action targets 24,
pooling 72 contexts per translation frequency while keeping individual actions
manageable. Trials are directly selectable and retain their per-trial progress,
so a focused follow-up can skip completed or low-priority motions. The completed
focused run collected 10 X-fast, 9 X-impulse, and 18 Y-medium contexts. Combined
with the earlier 24 X-slow contexts, it distinguished the constant fractional
position above from a frequency-dependent delay without requiring every trial
to reach its nominal quota.

The high-precision quaternion describes an older point in the same stream. With
normal quaternion keyframes assigned to the preceding normal packet terminal as
documented above, spherical interpolation places the class-`0xC` quaternion at
about two-thirds through the preceding normal interval. For the dominant
11-tick intervals the best aggregate fit is 3.5 ticks before the preceding
normal packet terminal; the exact two-thirds position is 3.67 ticks before it.
Median interpolation error is below `0.001` degrees. This explains why the
packet is closer to the following normal quaternion even though it is emitted
immediately after the preceding normal report.

The quaternion and filtered acceleration therefore do not share a sample
timestamp. A consumer may use the quaternion as a precision fusion keyframe at
the two-thirds point and expose the acceleration separately as a filtered sample
1.5 sensor ticks after the preceding normal terminal.

#### Bonded sleep/wake reset

The dedicated sleep capture confirms that the native fusion state is scoped to
one controller session. Immediately before sleep, the last normal packet had
sensor timestamp `3453` and an orientation `37.92` degrees from identity. The
first normal packet after bonded wake had timestamp `10` and was only `0.0063`
degrees from identity. The sensor clock and raw quaternion reference therefore
both restart on wake; a decoder must not bridge quaternion deltas or timestamp
history across the reconnect.

The physical sensor values did not reset. Class-`0xC` acceleration before and
after wake was approximately `[-20.91, -125.05, 4056.78]` and
`[-16.67, -125.11, 4055.23]` raw counts, and temperature remained exactly `512`
(`29 °C`) in Q8.7 form. The selector tuple also remained `[0, 2, 0]`, confirming
that the selector interpretation is format-driven rather than session state.

Polymer constructs a new physical report decoder for every `Connected` event,
so its timestamp history, orientation tracker, and stationary drift estimator
reset at the correct boundary. The shared motion decoder also treats a
non-continuous sensor-clock jump as a defensive session boundary, covering a
report/lifecycle race or a long unobservable gap. While disconnected, the
provider clears the published motion snapshot instead of exposing the final
pre-sleep orientation.

### Alternate 40-byte timing classes

Class `0xF` is an extended-interval form, normally covering 24 ticks. Its first
68 bits use the normal projective quaternion layout. It carries the same three
accelerometer/two-gyroscope chronology at these body offsets:

| Samples | Body offsets | Encoding |
| --- | --- | --- |
| Accelerometer earlier | 68, 82, 96 | signed 14-bit |
| Gyroscope earlier | 110, 126, 142 | signed 16-bit with two fractional bits |
| Accelerometer middle | 158, 171, 184 | signed 13-bit, `<< 1` |
| Gyroscope terminal | 197, 213, 229 | signed 16-bit with two fractional bits |
| Accelerometer terminal | 245, 259, 273 | signed 14-bit |

Divide the signed 16-bit gyro lanes by four before applying the normal physical
scale. The class has no temperature field. Body bit 287 mirrors the sign of the
most recently emitted temperature value: it matched that cached sign in all
186 class-`0xF` packets in the focused capture. This is consistent with the
encoder retaining the otherwise displaced temperature sign bit, rather than a
new class-`0xF` measurement, so the decoder continues to retain the complete
previous temperature. Across all 22,052 packets the decoded groups follow the
surrounding class-`0xE` stream,
with median component errors of roughly one to four raw counts at their
expected interpolation points.

For the common elapsed-24 class-`0xF` packet, the completed timing capture and
the larger guided-motion corpus independently resolve this schedule:

| Value | Age before terminal | Tick in the usual 24-tick packet |
| --- | ---: | ---: |
| Orientation | 20 ticks | 4 |
| Accelerometer earlier | 18 ticks | 6 |
| Gyroscope earlier | 10 ticks | 14 |
| Accelerometer middle | 8 ticks | 16 |
| Gyroscope terminal | 2 ticks | 22 |
| Accelerometer terminal | 0 ticks | 24 |

The elapsed-24 placement was solved by fitting each hidden class-`0xF` value
against the known samples on both sides of it. As a control, the same fit
recovered the documented class-`0xE` positions: gyro `8/12` for elapsed 12,
`8/11` for elapsed 11, and approximately `9/13` for elapsed 13. For elapsed-24
class-`0xF` packets, independent captures place acceleration at `6/16/24`, gyro
at `14/22`, and the orientation at tick 4. The IMU lanes retain terminal-relative
ages `18/8/0` for acceleration and `10/2` for gyroscope across the rare spans
that have enough usable motion. Orientation instead advances in timing bands:
tick 0 for elapsed 15, tick 4 for elapsed 22-26, and tick 12 for elapsed 35-37.
The one-way rare-span capture places elapsed 47-48 at tick 20, completing the
same eight-tick progression.

Class `0xD` is a high-precision continuation packet. Its elapsed value is always
9 or 10 ticks in the corpus. The stationary class-D experiment resolved the
whole body: each orientation lane stores a 21-bit coarse value with two
fractional bits, while each IMU lane stores a signed 14-bit coarse value with
eight fractional bits. The fractional bits precede their coarse field instead
of following it in ordinary integer order.

The body starts with the normal range flags in bits 0-1, the omitted quaternion
component in bits 2-3, and reserved bit 4. The three projective quaternion
coordinates are:

| Coordinate | Fraction bits | Coarse bits | Reconstruction |
| --- | ---: | ---: | --- |
| 0 | 5 (2 bits) | 7 (21 bits) | `(coarse << 2) | fraction` |
| 1 | 28 (2 bits) | 30 (21 bits) | `(coarse << 2) | fraction` |
| 2 | 51 (2 bits) | 53 (21 bits) | `(coarse << 2) | fraction` |

Subtract `0x400000` from each reconstructed unsigned 23-bit coordinate and
divide by `0x400000`, then unfold and normalize using the omitted-component
cycle shared with class `0xE`. Across the 48 stationary packets, this
orientation is a median `0.0141°` from the following class-`0xE` quaternion.
That agreement is expected because a normal packet's quaternion describes the
previous report interval; class `0xD` makes the same orientation available one
packet earlier and at four times the coordinate precision.

The nine signed Q14.8 sensor lanes follow immediately. For each lane, combine
the signed 14-bit coarse value and its preceding unsigned fraction as
`coarse + fraction / 256` before applying the normal range and physical-unit
scales:

| Samples | Fraction offsets | Coarse offsets |
| --- | --- | --- |
| Accelerometer first | 74, 96, 118 | 82, 104, 126 |
| Gyroscope terminal | 140, 162, 184 | 148, 170, 192 |
| Accelerometer terminal | 206, 228, 250 | 214, 236, 258 |

Temperature remains a signed 16-bit Q8.7 value at bit 272, relative to 25 °C.
The apparently unrelated 82-bit prefix and eight interleaved bytes were thus
the high-precision quaternion and the nine fractional IMU bytes; no unknown
payload remains in class `0xD` apart from reserved bit 4.

### Four-byte packet

The four-byte packet contains only the timing/header word. A decoder can safely
retain its last motion state when receiving either an undecoded 30-byte packet or
a four-byte packet.

Full 40-byte timing classes `0xD` and `0xF` must be dispatched to the separate
layouts above.

## Safe initial implementation

A safe decoder can now recover the observable motion stream:

1. Dispatch by both motion length and timing class.
2. Decode class `0xE` with the normal offsets and class `0xF` with the
   extended-interval offsets above.
3. Apply the independent accelerometer and gyroscope range multipliers from bits
   0-1 to every relevant sample group.
4. Assign sample timestamps from the elapsed-tick schedule above.
5. Decode the projective quaternion and timestamp it one normal report behind the
   packet's terminal IMU sample.
6. Decode class `0xC` as the high-precision projective quaternion and
   filtered acceleration described above. It is not a truncated normal packet.
7. Decode class `0xD`'s Q21.2 projective quaternion and Q14.8 IMU continuation,
   publish its orientation at the packet terminal time, and advance the sensor
   clock.
8. Retain the previous motion state for all other lengths and timing classes.

## Polymer implementation status

The shared Switch 2 native-motion decoder is implemented in
`crates/input/polymer-input-bluetooth`. Parsing and stabilization live in the
input provider rather than in bttest, so GUI and future virtual-device consumers
receive the same motion state. Every physical Switch 2 decoder owns its own
orientation tracker; the state is not shared between controllers.

All four observed packet classes are handled by this shared decoder. Class `0xE`
updates the raw three-accelerometer/two-gyroscope history. Class `0xC` updates
the precision quaternion, filtered Q16.16 acceleration, temperature, and sensor
timestamp while retaining the preceding raw history and marking it as not newly
sampled. Normal-to-normal sensor timestamp differences define the raw history
span even when a class-`0xC` packet splits the report stream. Class `0xF`
publishes its extended-interval history while retaining the previous
temperature. Class `0xD` publishes its complete high-precision quaternion and
fractional IMU continuation. This behavior is shared by Joy-Con
2, Pro Controller 2, and the NSO GameCube controller; `0x07` through `0x0A`
differ only in their surrounding native report envelope.

The controller's quaternion drifts while physically stationary, at roughly the
same rate as the non-zero stationary gyroscope bias. The provider suppresses it
with an adaptive stationary estimator:

1. A frame is considered stationary only when the terminal accelerometer is
   within `0.45 m/s²` of 1 g, both the terminal gyroscope magnitude and decoded
   quaternion angular rate are at most `0.035 rad/s`, and those conditions remain
   true for 500 ms.
2. On entering the stationary candidate state, the provider saves an orientation
   anchor. Once confirmed, it holds that anchor instead of allowing the raw
   quaternion to walk.
3. The local quaternion delta
   `conjugate(q_previous) * q_current` supplies a measured stationary drift rate.
   A per-axis exponential moving average with alpha `0.02` updates for as long as
   the controller remains still.
4. During motion, the estimated drift delta is removed from each raw local delta
   before it is accumulated into the stabilized orientation.

This continuously relearns bias as the sensor warms without applying a direct
temperature-to-bias formula. The decoded temperature remains useful for display,
capture analysis, and a future explicit temperature model. The estimator resets
when a new decoder/session is created and needs another stationary confirmation
after reconnecting.

bttest renders the stabilized orientation using the embedded `ngc.glb` model. A
viewer recenter stores only a display reference; it does not reset or alter the
provider's bias estimator. Relative rotation is calculated and rendered in the
native quaternion basis documented above.

### Switch 2 family reuse

The native controller-specific reports all expose the same observed motion
lengths (`0`, `30`, and `40`) and enable motion through feature bit 2. Polymer
now shares one `NativeMotionDecoder` across:

| Controller | Native report | Length byte | Motion start |
| --- | ---: | ---: | ---: |
| Joy-Con 2 (L) | `0x07` | `0x0F` | `0x10` |
| Joy-Con 2 (R) | `0x08` | `0x0F` | `0x10` |
| Pro Controller 2 | `0x09` | `0x0E` | `0x0F` |
| NSO GameCube controller | `0x0A` | `0x0E` | `0x0F` |

The packed class-`0xE` decoder was derived from the GameCube capture corpus, then
verified live with the Pro Controller 2 and Joy-Con 2 (R). Reports `0x08` through
`0x0A` therefore share the normal packed-body codec; only the motion envelope's
offset differs between the half-controller and full-controller layouts. Report
`0x07` uses the same Joy-Con envelope and shared implementation but has not yet
been independently exercised on a left Joy-Con. The shared decoder dispatches
30-byte class `0xC` and complete 40-byte classes `0xD`, `0xE`, and `0xF`
separately. Buttons and sticks remain available across short or transitional
motion packets.

Each physical decoder owns an independent orientation tracker so stationary
bias learned from one Joy-Con or controller cannot affect another.

## Remaining unknowns

The normal-class IMU and quaternion codec is solved for the observed 11-13 tick
normal stream. Remaining work is limited to:

1. Verification of the integer sample-position rule for normal elapsed values
   outside 11-13, should such packets occur without a stream discontinuity.
2. The purpose of reserved body bit 4. It has remained zero and does not block
   decoding.
3. Independent confirmation on Joy-Con 2 (L) report `0x07`.
No additional physical capture is needed to implement the shared normal decoder
or the decoded class-`0xC` quaternion body. Bonded sleep/wake session reset and
the rare class-`0xF` orientation bands have also been confirmed.

## Implemented bttest experiment

The motion-capture panel now has a **Run dual-report experiment** action. It
subscribes the common `0x05` and native `0x0A` characteristics concurrently and
observes them for three seconds. If both streams produce notifications, it keeps
the concurrent subscriptions active. Otherwise it switches to acknowledged,
automatic `0x05`/`0x0A` alternation at 250 ms intervals.

The controller continued to emit only `0x0A` while both characteristics were
subscribed, proving that subscription alone does not enable simultaneous report
streams. The fallback succeeded. WinRT characteristic switching added roughly
90 ms of overhead, so acknowledged switches were normally about 340 ms apart.

Capture schema `polymer.switch2-motion.v2` records:

- timestamped subscription, probe, fallback, and stop events;
- timestamped successful report-selection acknowledgements;
- the two originally suspected selector values on each complete native packet;
- live and final raw counts for values 0 through 3 at body bits 0 and 128.

Bits 0-1 are now confirmed as the independent accelerometer and gyroscope range
flags. Bit 128 is ordinary Gyroscope Y data. The experiment's raw counters for
these fields are retained for capture compatibility but are obsolete diagnostics.

The later guided quaternion experiment records explicit positioning, transition,
and hold markers. Its completed run is what established the projective coordinate
cycle, component convention, one-report quaternion delay, and intra-packet timing
schedule above.

The **guided 30-byte lane experiment** is synchronized directly to class-`0xC`
notifications. After the user arms a trial, one class-`0xC` packet marks its
start. The user may take an arbitrary amount of time to perform the requested
motion, then arms the next class-`0xC` packet as its endpoint. Three subsequent
class-`0xC` packets are captured while the endpoint pose is held still. The
sequence repeats five times for stationary, positive/negative quarter-turn,
closed-loop full-turn, and translation-without-rotation actions on each axis. A
single bonded sleep/wake trial tests whether the decoded fields reset with the
controller session. It is a session-state check and is not required to decode
the class-`0xC` body.

Each `lane_experiment` JSONL marker records the action and repetition, boundary
notification sequence, sensor timestamp, elapsed ticks, three two-bit lane
controls, raw 12-byte unresolved prefix, Q16.16 acceleration, and temperature.
This lets analysis address class-`0xC` boundaries without reconstructing them
from host-time proximity.

Bttest also exposes **Start sleep/wake capture…**, which runs only the final
bonded sleep trial. Its capture remains open across the intentional disconnect,
records transport disconnect/reconnect markers, and restores native report
selection after wake. The completed run established the session reset behavior
documented above.

Stopping the experiment or capture restores an exclusive native `0x0A`
subscription.

The **guided class-D experiment** advances on actual timing-class `0xD`
packets, not host-time estimates. It collects eight packets in each of six
stationary poses, slow positive/negative rotation about all three axes, three
flick actions, and translation without intentional rotation on each axis. The
user positions the controller first and starts each action explicitly; movement
happens only during the capture stage.

Each `class_d_experiment` marker records the notification sequence, complete
36-byte body, header timing, range and omitted-component controls, all directly
decoded sensor lanes, the 82-bit unresolved prefix, and the eight unresolved
interleaved bytes. The normal notification records immediately surrounding that
sequence retain the preceding class `0xC` and following class `0xE` context.

The six stationary actions completed with eight class-`0xD` packets apiece.
That corpus was sufficient to solve the layout without the moving actions. The
bits previously called unresolved form a regular fractional-first stream:
three `(2 fractional, 21 coarse)` orientation lanes followed by nine
`(8 fractional, 14 coarse)` IMU lanes. The old marker field names are retained
only for capture-schema compatibility.

The **bit 287 + packed timing experiment** captures sixteen complete class-`0xF`
contexts for each of initial and warmed stationary holds, steady pitch/roll/yaw,
mixed flicks, isolated taps, and an optional stationary-charging action. A
context contains the preceding, class-`0xF`, and following native packet
headers; their capture sequence numbers; both host-time gaps; both wrapped
sensor-timestamp gaps; the class-`0xF` final body byte; and decoded bit 287. Live
zero/one counts show immediately whether the bit changes under any manipulation.

The same observer creates a structured context for any class-`0xE` packet whose
elapsed field falls outside the observed 11-13 tick range, without counting it
toward the class-`0xF` action target. Complete raw notifications remain in the
JSONL file, so each context can be joined back to its full body. No artificial
egui stall is used: delaying the UI would only backlog bttest's channel while
the Bluetooth task continued draining WinRT, and therefore would not perturb
the controller's packed timing.

The completed focused run captured 128 guided and 186 total class-`0xF`
packets. Bit 287 was zero 98 times and one 88 times, ruling out constant padding.
It equaled the sign of the latest temperature from class `0xC`, `0xD`, or `0xE`
in all 186 cases. The run also confirmed, over all 19,048 consecutive native
packet transitions, that `elapsed_ticks` is exactly the wrapped difference
between successive 12-bit sensor timestamps. Class `0xE` produced only elapsed
values 11, 12, and 13, so its extrapolated sample-position rule outside that
range was not exercised. Class `0xF` produced 175 spans of 24 ticks plus sparse
spans of 15, 22, 23, 35, and 37 ticks; those rare spans are preserved for
further intra-packet interpolation analysis.

The **rare-span timing experiment** targets elapsed values `15`, `22`, `23`,
`25`, `26`, `35`, `36`, `37`, `47`, and `48` separately during smooth pitch,
roll, and yaw. Each quota cell requires ten valid contexts. A context is accepted
when the collector can find the nearest two full class-`0xE` anchors before and
after the class-`0xF` focus within a bounded sensor-clock window. Every intervening
packet is retained, including short class-`0xC` packets, class-`0xD` continuations,
and adjacent class-`0xF` packets. Candidates remain pending concurrently, so one
rare packet cannot evict another. Elapsed-15 packets are accepted only when their
first following full packet is the observed class-`0xD`, elapsed-9 continuation.
The structured marker contains the four anchors, focus, optional continuation,
explicit before/after bridges, the complete ordered timeline, and host-time and
wrapped sensor-clock deltas for every adjacent timeline pair. Raw notification
records retain the full payloads used for interpolation.

The UI permits pausing and changing axes without carrying a partial context
across the manipulation boundary. Live rows distinguish observed candidates
from complete anchored contexts. Moving farther from the BLE adapter or
partially shielding the controller can encourage elapsed-35 through elapsed-48
packets without blocking WinRT. An explicit native-`0x0A` re-selection probe is
available while motion continues to exercise the suspected elapsed-15 report
transition. No artificial application stall is used.

The first capture with the widened collector produced 137 distinct complete
contexts (the UI showed 134 shortly before the last pending contexts completed),
with only two rejected candidates. It recovered three elapsed-15 contexts, one
elapsed-47 context, and seven elapsed-48 contexts. All three elapsed-15 packets
were followed by a full class-`0xD`, elapsed-9 packet, confirming that this is a
real two-packet `F15 -> D9` transition and not accidental adjacency.

Smooth, same-direction portions of the capture settle the class-`0xF`
orientation schedule through elapsed 37. Elapsed 15 places orientation at tick
zero of its span. Elapsed 22-26 places it at tick 4, while elapsed 35-37 places
it at tick 12. The previously recovered terminal-relative gyroscope ages of 10
and 2 ticks remain consistent across those bands. That first run left elapsed
47-48 provisional because several long contexts contained hand-motion reversals.
A second one-way capture produced 481 complete contexts, including 23 elapsed-48
packets across pitch, roll, and yaw. A quaternion Hermite fit constrained by the
decoded gyroscope converged at about 28 ticks before the elapsed-48 terminal,
placing orientation at tick 20. This matches the eight-tick progression already
established by the shorter bands and closes the final rare orientation placement.

Switch 1 does not provide an older instance of this codec. Its Joy-Con reports
`0x30` through `0x33` transmit three uncompressed `i16` accelerometer/gyro
samples and no controller-computed quaternion. The Switch `btdrv` service
delivers raw HID reports to the `hid` sysmodule, so a Switch 2 reference decoder
would be expected in `hid`, not in the Bluetooth transport module.

## References

- Nintendo orientation-data compression patent:
  <https://uspto.report/patent/grant/11009334>
- Current public Switch 2 HID report notes, which still mark the packed motion
  format as unknown:
  <https://github.com/ndeadly/switch2_controller_research/blob/master/hid_reports.md>
- Switch 1 Joy-Con IMU report layout:
  <https://github.com/dekuNukem/Nintendo_Switch_Reverse_Engineering/blob/master/imu_sensor_notes.md>
- Switch Bluetooth-driver service boundary and raw HID report delivery:
  <https://switchbrew.org/wiki/Bluetooth_Driver_services>
- TDK ICM-42670-P data sheet, including the temperature conversion:
  <https://product.tdk.com/system/files/dam/doc/product/sensor/mortion-inertial/imu/data_sheet/ds-000451-icm-42670-p.pdf>
