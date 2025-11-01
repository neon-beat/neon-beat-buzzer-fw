# Neon Beat Buzzer

This repository contains the Neon Beat Buzzers firmware, written in no_std
rust

## Project generation

The project has been generated thanks to
[esp-generate](https://github.com/esp-rs/esp-generate) with the following
command:

```sh
$ esp-generate --headless -c esp32c3 -o unstable-hal -o alloc -o wifi -o embassy neon-beat-buzzer
```

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
