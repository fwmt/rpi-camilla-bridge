# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.2] - 2026-05-10

### Added

- `pi-receiver init` interactive wizard. Reads `/proc/asound/cards`,
  filters out the loopback (so the speaker-protection guard is never
  asked to swallow a self-routed config), prompts for channels and
  sample rate, and writes a starter `bridge.yml` derived from the
  shipped template. Three swap-points: the playback `hw:CARD=…` line,
  the playback channel count, and the capture/playback samplerate.
  Filters / mixers / pipeline are kept as the empty-passthrough so the
  user adds their own protection chain — never auto-fills gain values.
- `pi-receiver doctor` health-check subcommand. Walks the eight failure
  modes that actually waste time in support tickets — snd-aloop loaded,
  Loopback card visible, a DAC present, CamillaDSP websocket reachable,
  bridge / idle configs readable and plausibly-shaped, the `--device`
  flag passes the loopback guard, and a UDP 5353 listener exists for
  mDNS. Each check prints one line (✓ / ! / ✗); exits non-zero if any
  fails, so it slots into shell scripts. Read-only — never mutates state.

### Changed

- GitHub Actions workflows bumped to action versions that ship Node 24
  internally (checkout v6, cache v5, upload-artifact v7, download-artifact
  v8, mise-action v4, action-gh-release v3) — clears the Node 20
  deprecation notice GitHub started attaching to every CI / release run.

## [0.1.1] - 2026-05-10

### Added

- mDNS / DNS-SD auto-discovery. `pi-receiver` publishes
  `_camilla-bridge._tcp.local.` on every interface (auto-refreshing as
  Wi-Fi / Ethernet come and go); `pc-sender` invoked without `--host`
  browses the LAN for ~5 s and connects to whatever Pi answers. If
  multiple Pis answer, `pc-sender` lists them and asks the user to
  pick one with `--host <pi>.local`. Tolerant on both sides — if the
  daemon can't start, the receiver still serves on TCP and the sender
  still accepts an explicit host.
- `pc-sender` connects to the discovered Pi via its mDNS hostname
  (e.g. `hifiberry.local`) rather than the first announced IP. The
  OS resolver picks a working address — important on Pis with multiple
  interfaces (eno1, docker bridges, libvirt bridges) where
  `enable_addr_auto()` would otherwise advertise every one of them.

### Fixed

- `install-pi.sh` no longer dies with `tmpdir: unbound variable` at the
  end of a successful run. The scratch directory was scoped to the
  download function but referenced from a script-level EXIT trap;
  promoted it to a script-scope `WORK_DIR` with a defensive `cleanup`.
- `release.yml` workflow extracts the matching CHANGELOG section
  literally (`index($0, header)` instead of regex) so the release body
  isn't truncated to the generic fallback when the version contains
  characters awk treats as a regex character class.

## [0.1.0] - 2026-05-10

First public release. PC ↔ Raspberry Pi audio bridge over TCP, feeding
CamillaDSP through `snd-aloop` so the existing protections (limiters,
crossovers, gain staging) always sit between the wire and the DAC.

### Added

#### Core

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

#### End-user UX

- `pc-sender` on Linux registers a PulseAudio / PipeWire null-sink at
  startup so it appears as a regular speaker in
  *Settings → Sound → Output*. Default label is `Raspberry Pi (<host>)`,
  derived from `--host` (with `.local` stripped). Customizable via
  `--output-name "Living Room"`. The sink is unloaded and the previous
  default source restored on exit. Suppressed by `--no-virtual-sink`
  or by passing an explicit `--device`.
- `pc-sender` prints friendly status lines instead of structured logs
  by default: `✓ Audio output … is now in your Sound settings`,
  `✓ Streaming to host:port — S32LE / 48000 Hz / 2ch`,
  `✓ Disconnected cleanly (1.1 MiB)`. ANSI color when stdout is a TTY;
  suppressed by `NO_COLOR=1`. `--verbose` (or `--log-level=…`) brings
  the structured tracing back for bug reports.

#### Distribution

- `install-pi.sh`: one-line installer
  (`curl … | sudo bash`) that downloads the latest published binary,
  verifies the SHA-256, creates the `pi-camilla` system user, drops the
  `bridge.yml.example`, prompts for the path to the user's existing
  CamillaDSP config (symlinked as `idle.yml`), installs the systemd
  unit, and starts the service.
- GitHub Actions release pipeline (`.github/workflows/release.yml`)
  triggered by tags. Builds in parallel: `pi-receiver` for
  aarch64-linux (cross), `pc-sender` for x86_64-linux,
  x86_64-pc-windows-msvc, aarch64-apple-darwin. Each artifact bundles
  the binary, README, both LICENSE files, the CHANGELOG and (for
  pi-receiver) the deploy/ scaffolding. SHA-256 alongside each.
  Release body sourced from this CHANGELOG.

#### Project scaffolding

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
- `.github/workflows/ci.yml`: on every PR and push to main, runs
  `mise run ci` and produces an aarch64 build artifact.
- `.github/ISSUE_TEMPLATE/{bug_report,feature_request,config}.yml`,
  `.github/pull_request_template.md`, `CONTRIBUTING.md`, `CHANGELOG.md`.
- Dual MIT / Apache-2.0 licensing.

[Unreleased]: https://github.com/fwmt/rpi-camilla-bridge/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/fwmt/rpi-camilla-bridge/releases/tag/v0.1.2
[0.1.1]: https://github.com/fwmt/rpi-camilla-bridge/releases/tag/v0.1.1
[0.1.0]: https://github.com/fwmt/rpi-camilla-bridge/releases/tag/v0.1.0
