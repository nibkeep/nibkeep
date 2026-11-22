# The power switch of the power-cut rig

A [Seeed XIAO ESP32-C6](https://wiki.seeedstudio.com/xiao_esp32c6_getting_started/) that cuts and restores the 5 V supply of a Raspberry Pi on command over Wi-Fi, so that `scripts/power-cut.py --switch auto` can cut the power under a library hundreds of times without anyone pulling a plug.

## The circuit

The switch is on the 5 V line (high side), so that the Pi's ground stays connected. A P-channel MOSFET carries the Pi's current; an NPN transistor, driven by the ESP32's 3.3 V pin, pulls its gate down.

```text
 5.2 V supply +5V ──┬──────────── S ┐
                    │               │ DMP3010LK3 P-channel MOSFET, logic level
                 5.1 kΩ        G ───┤
                    │          │    │
                    └──────────┤    └ D ──────► Pi +5V (the USB cable's red wire)
                               │
                               C ┐
                                 │ 2N2222A NPN transistor (only switches the gate: milliamps)
 XIAO D0 (GPIO0) ── 1 kΩ ─┬─ B ──┤
                         10 kΩ   └ E ──┐
                          │            │
 GND ─────────────────────┴────────────┴────────────► Pi GND (never switched)
 (the supply's, the XIAO's and the Pi's ground joined)
```

- D0 high: the transistor pulls the MOSFET gate to ground, the MOSFET conducts, and the Pi is powered. D0 low, or the XIAO starting: the 5.1 kΩ resistor holds the gate at +5 V, and the Pi is off.
- The P-channel MOSFET must be logic level: its on-resistance (R_DS(on)) specified at a gate voltage of −4.5 V, below about 0.1 Ω, for at least 3 A. MOSFETs specified only at −10 V, such as the IRF9540 and IRF4905, do not switch fully.
- The program selects the XIAO's external antenna, so one must be attached: without it the board barely hears the router and may fail to join with `AuthenticationExpired`.
- The XIAO takes its power from the same supply on its 5V pin, before the MOSFET, so that it stays on when it cuts the Pi. Powered from the Pi, it would switch itself off with it. Before plugging in the XIAO's USB-C port, such as to flash it, take the supply off that pin, unless the board has a diode between the two: otherwise the supply and the USB port's 5 V meet.
- The supply gives 5.2 V, since the wires and connections to the Pi lose some of it. At about 5 V the Pi reports undervoltage under load: the XIAO's 5V pin, with the XIAO on a USB charger, gives only 4.95 V, too little to power the Pi from.
- Feed the Pi through its USB power input, not its GPIO 5 V pins, which bypass its fuse.

## The program

At start-up it powers the Pi, joins the Wi-Fi network, and prints its address over USB. A small HTTP server on port 80 answers:

| Request | Does |
| --- | --- |
| `GET /status` | Says `nibkeep power switch: on` or `off` |
| `POST /on` | Powers the Pi |
| `POST /off` | Cuts the Pi's power |

If the power stays cut for ten minutes, the switch restores it, so that a test that dies never leaves the Pi off for good. The board's LED is lit while the Pi is powered.

It is written in Rust, with [esp-hal](https://github.com/esp-rs/esp-hal), esp-radio and Embassy, and builds with stable Rust for the target `riscv32imac-unknown-none-elf`.

## Building and flashing

The Wi-Fi network's name and password go in `wifi.env`, next to `Cargo.toml`, which Git ignores:

```text
WIFI_SSID=...
WIFI_PASSWORD=...
```

The ESP32-C6 only uses 2.4 GHz networks. Then, with the XIAO on USB:

```sh
cargo run --release
```

which builds, flashes with [espflash](https://github.com/esp-rs/espflash), and shows the program's output. A XIAO plugged into another computer, such as the Pi next to it, is flashed there: copy `target/riscv32imac-unknown-none-elf/release/power-switch` to it and run `espflash flash --chip esp32c6 --monitor power-switch`.
