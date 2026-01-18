# Neon Beat Buzzer

This repository contains the Neon Beat Buzzers firmware, written in no_std
rust. This firmware aims to run on an esp32c3 target to support the
following features involved in a Neon Beat game:
- connecting to the Neon Beat controller (NBC) over wifi, and then
  connecting to the corresponding websocket server
- listening to button pushes on a specific gpio, and sending push
  notifications to the NBC
- receiving, parsing and executing led commands to display the buzzer
  status on a ws2812 led, wired to a single gpio

## Project status

This Neon Beat Buzzer is still under active development. The current scope
of the code base supports almost all needed features for a Neon Beat game.
There is still some pending tasks:
- some minor features are currently not supported, e.g interpreting some
  specific pattern duration/period/duty cycle for leds
- the code base will still receive quite some refactoring:
  - more idiomatic Rust
  - better configuration management
  - better error handling
  - etc
- there is currently almost no developper or user doc
- there is no CI automation

The project is actually a Rust rewrite of a former C firmware which can be
found at https://github.com/tropicao/neon-beat-buzzer
## Build and run the project

- install [rustup](https://rustup.rs/)
- install the needed toolchain thanks to `rustup`:
```sh
$ rustup target add riscv32imc-unknown-none-elf
```
- install [espflash](https://github.com/esp-rs/espflash/):
```sh
$ cargo install espflash --locked
```
- plug a Neon Beat Buzzer into your computer through a USB C cable
- build the project and flash the buzzer:
```sh
$ cargo run
```

## Developpers' notes

The project has been generated thanks to
[esp-generate](https://github.com/esp-rs/esp-generate) with the following
command:

```sh
$ esp-generate --headless -c esp32c3 -o unstable-hal -o alloc -o wifi -o embassy -o log neon-beat-buzzer
```

The project is using the log crate coupled with esp_println. The firmware
only outputs by default logs down to info level. The log levels can be
tuned when re-flashing the buzzer:
- to get the main application debug logs:
```sh
ESP_LOG=neon_beat_buzzer cargo run
```
- to get ALL the debug logs (very verbose, as it includes all the debug
  logs from any component):
```sh
ESP_LOG=debug cargo run
```

