# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-05-10

### Added

- `proto` crate: 16-byte wire header (CDSP magic, version, format
  S16/S32/F32 LE, channels, sample rate). Encoder, decoder, exhaustive
  unit tests for roundtrip and rejection paths.
- `pi-receiver` daemon: TCP listener, ALSA playback into the snd-aloop
  card that CamillaDSP captures from, websocket client that swaps
  CamillaDSP between a bridge config (active while a PC source is
  connected) and an idle config (restored on disconnect). Open-with-retry
  loop accommodates CamillaDSP reload latency. Underrun recovery via
  `pcm.try_recover`. Speaker-protection guard rejects any device whose
  name lacks `loopback` (case-insensitive) or matches a banned DAC alias
  (`hifiberry`, `dac`, `default`, `plug*`, `pulse`, `pipewire`, …) with
  no CLI escape hatch.
- `pc-sender` client: cpal capture with quality-ranked sample format
  selection (F32 > I32 > I16 > U16 > U8 > I8), TCP write with reconnect
  backoff bounded at 5 s, `list-devices` subcommand. Graceful shutdown
  in ~110 ms via `recv_timeout`-polling writer plus a TCP `shutdown(Both)`
  watchdog that unblocks `write_all` on Ctrl+C.
- `deploy/bridge.yml.example`: minimal stereo passthrough template.
- `deploy/pi-receiver.service`: systemd unit running as a dedicated
  unprivileged `pi-camilla` user.
- `deploy/README.md`: deployment workflow and `/etc/rpi-camilla-bridge/`
  layout.
- `examples/bridge-stereo-2.3-real-world.yml`: working production config
  with bass management, gain staging, soft-clip limiters and 2-in / 8-out
  matrix mixer (structural reference; gain values are hardware-specific).
- `mise.toml` toolchain pin (rust 1.95 dev, MSRV 1.85, edition 2024) and
  task pipeline: `setup`, `fmt`, `fmt-check`, `lint`, `test`, `audit`,
  `unused-deps`, `build`, `build-pi`, `watch`, `ci`.
- `Cross.toml`: cross-compile config for Pi 5 (aarch64) with
  `libasound2-dev` installed inside the Docker image.
- `deny.toml`: cargo-deny config (advisories deny-yanked, license
  allowlist).
- Dual MIT / Apache-2.0 licensing.

[Unreleased]: https://github.com/fwmt/rpi-camilla-bridge/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/fwmt/rpi-camilla-bridge/releases/tag/v0.1.0
