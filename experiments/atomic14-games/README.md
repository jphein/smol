[![Build ESP32-C3 Games](https://github.com/atomic14/esp32-c3-oled-single-button-games/actions/workflows/build-esp32-c3-games.yml/badge.svg)](https://github.com/atomic14/esp32-c3-oled-single-button-games/actions/workflows/build-esp32-c3-games.yml)

# ESP32-C3 OLED Single-Button Games

This project is a collection of fairly crappy tiny games for the ESP32-C3 0.42in OLED module.
The display is treated as a `70x40` game area (within the panel's `72x40` pixels),
and all games are designed around a single button input on `GPIO9` (BOOT).

It's all been vibe coded - so quality is questionable!

You can pick up one of these cheap modules [here](https://s.click.aliexpress.com/e/_c3HD7IPz).

![Rubbish Games](docs/games.webp)

## Controls

- `Tap` in menu: move to next game
- `Long press` in menu, then release: launch selected game
- In game start screen: `tap` to begin
- In game over screen: `tap` to restart, `long press` to return to menu

## Current Games

- `Breakout` - one-button paddle direction switching
- `Micro Racer` - tap to lane switch
- `Defender Mini` - tap to switch altitude band, auto-fire
- `Jump Run` - tap to jump over obstacles
- `Heli Cave` - hold to rise, release to fall

## Architecture

- `Game` base class provides:
  - button event handling (`pressed`, `released`, `click`, `longPress`)
  - shared game state machine (`Start`, `Running`, `End`)
  - common lifecycle hooks for concrete games
- Each game lives in its own `*.h` and `*.cpp` and implements only running logic/drawing.
- `esp32-c3-games/esp32-c3-games.ino` handles display init, menu, and game switching.

## Build and Upload

Use Arduino IDE, or `arduino-cli`:

```sh
arduino-cli compile --fqbn esp32:esp32:esp32c3 esp32-c3-games
arduino-cli upload --fqbn esp32:esp32:esp32c3 --port /dev/cu.usbmodem1101 esp32-c3-games
```

## CI

GitHub Actions builds the sketch on push/PR with:

```sh
arduino-cli compile --fqbn esp32:esp32:esp32c3 esp32-c3-games
```