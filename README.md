# rpi-camilla-bridge

[![ci](https://github.com/fwmt/rpi-camilla-bridge/actions/workflows/ci.yml/badge.svg)](https://github.com/fwmt/rpi-camilla-bridge/actions/workflows/ci.yml)
[![license: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-orange.svg)](Cargo.toml)

Stream PCM audio from a Linux/Windows PC to a Raspberry Pi running
[CamillaDSP](https://github.com/HEnquist/camilladsp) over your LAN. The
PC's audio lands inside an ALSA loopback that CamillaDSP captures from, so
your existing CamillaDSP filters / limiters / crossovers stay in the path
all the way to the DAC. Nothing else.

```
[PC: Windows or Linux]                          [Raspberry Pi + ALSA DAC]
    cpal capture                                  pi-receiver daemon
         │                                              │
         └──── TCP, raw PCM (16 B header + frames) ────►│
                                                        ▼
                                              ALSA hw:Loopback,0,0
                                                        │ (snd-aloop mirrors)
                                                        ▼
                                              hw:Loopback,1,0
                                                        │
                                                        ▼
                                                  CamillaDSP
                                                  (filters / limiters /
                                                   mixer / resampler)
                                                        │
                                                        ▼
                                                     your DAC
```

While a PC source is connected, the receiver flips CamillaDSP's active
config via its websocket so the capture format matches what the wire is
producing. On disconnect it restores your idle config (whatever you were
running before — Tidal Connect, Roon, mpd, etc.).

On Linux PCs (PulseAudio / PipeWire), `pc-sender` registers itself as a
virtual output called **Raspberry Pi (\<host\>)** so it shows up in
*Settings → Sound → Output* as a regular speaker. Pick it there and any
app — browser, Spotify, mpv, a DAW — gets routed straight to your Pi's
DAC through CamillaDSP. Quit `pc-sender` and the virtual output
disappears, your previous default is restored.

## Why this exists

Most "PC audio to Pi" stacks (Snapcast, PulseAudio over network, AirPlay,
RAAT, RTP) bring their own daemon, their own protocol, their own format
conversions. If you already have CamillaDSP doing the heavy lifting on the
Pi for limiters / room correction / bass management, you don't want
another resampler in the path. This bridge is the minimum that gets PC
audio into the loopback CamillaDSP already captures from. Raw PCM, plain
TCP, 16-byte header. Less than 1000 lines of Rust on each side.

## Hardware assumptions

- Raspberry Pi (any model where you'd run CamillaDSP; tested on Pi 5).
- An ALSA-compatible DAC connected to the Pi.
- The `snd-aloop` kernel module loaded (`dtoverlay=snd-aloop` in
  `/boot/firmware/config.txt`, or `modprobe snd-aloop`).
- CamillaDSP already configured to capture from the loopback (e.g.
  `hw:CARD=Loopback,DEV=1`) and produce playback to your DAC. The bridge
  does not install or configure CamillaDSP — that's a prerequisite.
- A PC (Windows or Linux) on the same LAN as the Pi.

If your existing CamillaDSP setup feeds Tidal Connect, Roon, mpd, etc.
through `snd-aloop`, you already have everything except the receiver
binary and a second config file.

## Pre-requisites

- [`mise`](https://mise.jdx.dev/) for the toolchain. Install with
  `curl https://mise.run | sh` and activate per your shell's instructions.
- Docker (only needed for the cross-build to the Pi). Native build doesn't
  need it.
- On any **Linux** host that compiles `pi-receiver` or `pc-sender` natively,
  ALSA development headers must be present:

  ```sh
  sudo apt-get install libasound2-dev    # Debian / Ubuntu / Pop!_OS
  sudo dnf install alsa-lib-devel        # Fedora
  sudo pacman -S alsa-lib                # Arch
  ```

  Cross-builds via `mise run build-pi` install ALSA inside the Docker
  image automatically (see `Cross.toml`); no host install needed for that
  path.
- **Windows** needs only the standard MSVC toolchain that mise/rustup
  pulls.

## Setup

```sh
mise run setup        # installs Rust, cross, nextest, deny, machete, bacon, pnpm,
                      # then `rustup target add aarch64-unknown-linux-gnu`
```

## Workflow

| Task               | Command                |
| ------------------ | ---------------------- |
| Build (native)     | `mise run build`       |
| Build (Pi aarch64) | `mise run build-pi`    |
| Format             | `mise run fmt`         |
| Format check       | `mise run fmt-check`   |
| Lint (clippy -D)   | `mise run lint`        |
| Tests (nextest)    | `mise run test`        |
| Audit (cargo-deny) | `mise run audit`       |
| Unused deps        | `mise run unused-deps` |
| Watch              | `mise run watch`       |
| Full local CI      | `mise run ci`          |

`mise run ci` chains `fmt-check → lint → test → audit → unused-deps`.

## Wire protocol

16-byte fixed header, sent once at the start of the TCP connection,
followed by an uninterrupted stream of interleaved little-endian PCM
frames in the declared format.

| offset | size | field                                     |
| ------ | ---- | ----------------------------------------- |
| 0      | 4    | magic = `b"CDSP"`                         |
| 4      | 1    | version = `1`                             |
| 5      | 1    | format (0=S16LE, 1=S32LE, 2=F32LE)        |
| 6      | 2    | channels (u16 LE)                         |
| 8      | 4    | sample_rate (u32 LE)                      |
| 12     | 4    | reserved (u32 LE, sender writes zero)     |

No per-frame framing, no checksum, no timestamp. TCP is the integrity
guarantee. See `proto/src/lib.rs` for the encoder/decoder.

## Wire contract with CamillaDSP

The PC sender's `--format / --rate / --channels` MUST match the
`capture` section of whatever bridge config you're using on the Pi. The
defaults shipped in `deploy/bridge.yml.example` are:

- `format: S32LE`
- `capture_samplerate: 48000`
- `channels: 2`

These match the `pc-sender` defaults (`--format s32le --rate 48000
--channels 2`). 48 kHz is the shared-mode mixer rate on most Windows
(WASAPI) and Linux (PipeWire / PulseAudio) systems, so it works
out-of-the-box on the great majority of PCs.

If your PC is configured at 44.1 kHz, 96 kHz or another rate, run:

```sh
pc-sender list-devices    # shows supported rate ranges per device
```

…then either change Sound settings to 48 kHz or override both ends in
lockstep:

1. Edit your bridge config's `capture.format` / `capture_samplerate` /
   `capture.channels`.
2. Pass the matching `--format / --rate / --channels` on `pc-sender`.

CamillaDSP's internal rate stays at whatever you configure in the
`devices.samplerate` field; its AsyncSinc resampler upsamples the wire
rate transparently. **There is no PC-side resampling.**

## Speaker-protection guard

`pi-receiver` refuses to open any ALSA device whose name does not contain
`loopback` (case-insensitive). Patterns like `default`, `null`,
`pulse`, `pipewire`, anything starting with `plug*`, and known DAC card
names are rejected with explicit errors. This is fail-closed and has no
CLI escape hatch — bypassing CamillaDSP is the one failure mode that
could destroy a loudspeaker, so the binary will not let you do it. If
you genuinely need to debug-write to a non-loopback device, edit
`pi-receiver/src/main.rs::enforce_loopback_only` in your fork.

## Quick start (smoke test)

After `mise run setup` and a build:

```sh
# 1. Adapt the example bridge config to your DAC.
cp deploy/bridge.yml.example /tmp/bridge.yml
$EDITOR /tmp/bridge.yml          # set playback device, channels, etc.

# 2. Push to the Pi.
ssh <user>@<pi-host> 'sudo install -d /etc/rpi-camilla-bridge'
scp /tmp/bridge.yml <user>@<pi-host>:/tmp/bridge.yml
ssh <user>@<pi-host> '
    sudo install -m 0644 /tmp/bridge.yml /etc/rpi-camilla-bridge/bridge.yml
    sudo ln -sf "$(realpath ~/camilladsp/configs/your-current-config.yml)" \
        /etc/rpi-camilla-bridge/idle.yml'

# 3. Cross-build the receiver and ship it.
mise run build-pi
scp target/aarch64-unknown-linux-gnu/release/pi-receiver \
    <user>@<pi-host>:/tmp/

# 4. Install systemd unit and start.
ssh <user>@<pi-host>
sudo useradd --system --no-create-home --shell /usr/sbin/nologin -G audio pi-camilla
sudo install -m 0755 /tmp/pi-receiver /usr/local/bin/pi-receiver
sudo install -m 0644 deploy/pi-receiver.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now pi-receiver
journalctl -u pi-receiver -f

# 5. From the PC.
pc-sender --host <pi-host> --port 9000
```

While `pc-sender` is running, CamillaDSP runs your bridge config. When
you Ctrl+C the sender, it exits within ~110 ms and CamillaDSP swaps back
to your idle config — your previous source (Tidal Connect, etc.) can
resume immediately.

`pc-sender` reconnects automatically on transient TCP failures. Useful
flags (defaults in parentheses):

- `--device <name>` (`default`) — case-insensitive match. Use
  `pc-sender list-devices` to see candidates.
- `--cpal-host <name>` — pin a cpal backend (e.g. `wasapi`, `alsa`).
- `--rate / --format / --channels` — wire format. Must match your bridge
  config's `capture` block.
- `--reconnect-ms <ms>` (`1000`) — initial reconnect delay; doubles up
  to 5 s.

## Examples

`examples/bridge-stereo-2.3-real-world.yml` — a working production config
for a stereo 2.3 setup with bass management, gain staging, soft-clip
limiters and Linkwitz-Riley crossovers. Use it as a structural reference
for what a richer pipeline looks like; **do not copy the gain values**
verbatim — they are tuned for a specific amplifier + speaker chain.

PRs adding `examples/*` for other channel layouts (2.0, 2.1, 5.1, 7.1)
or DAC families are welcome.

## Troubleshooting

- **No sound at all.** Verify the loopback is exposed and CamillaDSP
  captures from the right side:

  ```sh
  aplay -L | grep -i loopback
  cat /proc/asound/Loopback/pcm0p/sub0/status   # while pc-sender is running
  cat /proc/asound/Loopback/pcm1c/sub0/status
  ```

  `pcm0p` (playback half — what we write to) should be `RUNNING` while
  the PC is connected, owned by `pi-receiver`. `pcm1c` (capture half)
  should be `RUNNING`, owned by `camilladsp` reading our audio.

- **`warn: ALSA writei error → try_recover` repeating.** Underrun. Bump
  `--buffer-ms` (e.g. 300, 500). Check the LAN — wired gigabit absorbs
  6 Mbps at S32/96k easily, but Wi-Fi with contention is unreliable.

- **`Connection refused`.** `pi-receiver` not running, firewall blocking
  9000, or wrong `--host`. `nc -vz <pi-host> 9000` from the PC.

- **Sample rate mismatch / silence even though TCP is flowing.** The
  wire format doesn't match the bridge config's `capture`. Run
  `pc-sender list-devices`, confirm what your input device is producing,
  align both ends.

- **`config swap failed` warning at session start/end.** The websocket
  call to CamillaDSP failed (wrong host/port, CamillaDSP restarting).
  Audio still flows under whichever config CamillaDSP is currently
  running. Check with `systemctl status camilladsp` and
  `--camilla-host / --camilla-port`.

- **Windows captures the microphone, not system audio.** Native WASAPI
  loopback is on the roadmap but not yet wired up. For now: install
  [VB-CABLE](https://vb-audio.com/Cable/) (free), set it as your default
  output in Sound settings, and run `pc-sender --device "CABLE Output"`.
  Audio routed to VB-CABLE then flows through the bridge instead of your
  speakers. Some Windows machines also expose a built-in "Stereo Mix"
  capture device — `pc-sender list-devices` will show it if so.

- **Other source (Tidal, Roon) stops working while bridge is connected.**
  Expected — both feed the same `hw:Loopback,0`. The bridge config is
  reverted to your idle config automatically when `pc-sender`
  disconnects, so the other source can resume after that.

## Layout

```
.
├── mise.toml                        # toolchain + tasks
├── Cargo.toml                       # workspace root
├── Cross.toml                       # cross-compile config (Pi aarch64 + libasound2-dev)
├── deny.toml                        # cargo-deny: advisories + license allowlist
├── .config/nextest.toml             # test runner profile
├── proto/                           # wire header / format enum / errors
├── pi-receiver/                     # daemon on the Pi (TCP + ALSA + WS to CamillaDSP)
├── pc-sender/                       # PC client (cpal capture + TCP)
├── deploy/
│   ├── bridge.yml.example           # minimal template for bridge config
│   ├── pi-receiver.service          # systemd unit
│   └── README.md                    # deploy instructions
└── examples/
    └── bridge-stereo-2.3-real-world.yml   # full pipeline example
```

## Contributing

PRs welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for the
expectations: `mise run ci` green, scope kept narrow, real-world
configs documented and never copied verbatim across hardware. Bug
reports go through the issue template — it asks for the topology
fields that make audio bugs diagnosable.

## License

Dual-licensed under either of [Apache-2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT) at your option. Contributions are accepted under the
same dual license unless explicitly stated otherwise.
