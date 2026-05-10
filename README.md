# rpi-camilla-bridge

Stream PCM audio from a Linux/Windows PC to a Raspberry Pi 5 running
CamillaDSP, over the local network. The Pi hosts a HiFiBerry StudioDAC8x and
already runs CamillaDSP capturing from `snd-aloop`. This project delivers the
PC's audio into that loopback so CamillaDSP processes it before it reaches
the DAC. Nothing else.

```
[PC: Windows or Linux]                          [RPi 5 + StudioDAC8x]
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
                                                  (already running,
                                                   filters / limiters /
                                                   mixer / resampler)
                                                        │
                                                        ▼
                                              DAC8x → 8 analog outputs
```

The receiver also flips CamillaDSP's active config via its websocket while a
PC is connected, so the capture format matches what the PC is sending.
On disconnect it restores the idle config (your `music.yml`, normally fed by
Tidal Connect via the same loopback).

## Out of scope

No sample-rate conversion in the wire path (CamillaDSP does it, with
AsyncSinc), no clock-drift adaptation, no compression, no auth, no multi-
client, no mDNS discovery, no GUI, no async runtime. LAN-only, deliberate.

## Pre-requisites

- `mise` for the toolchain. Install with `curl https://mise.run | sh` and
  activate it per your shell's instructions.
- Docker (only needed for the cross-build to the Pi). Native build doesn't
  need it.
- On any **Linux** host that compiles `pi-receiver` or `pc-sender` natively,
  the ALSA development headers must be present:

  ```sh
  sudo apt-get install libasound2-dev    # Debian / Ubuntu / Pop!_OS
  ```

  Cross-builds via `mise run build-pi` install ALSA inside the Docker image
  automatically (see `Cross.toml`); no host install needed for that path.
- **Windows** needs only the standard MSVC toolchain that mise/rustup pulls.

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

`mise run ci` chains `fmt-check → lint → test → audit → unused-deps`. This is
what gates a release-ready state.

## Wire protocol

16-byte fixed header, sent once at the start of the TCP connection, followed
by an uninterrupted stream of interleaved little-endian PCM frames in the
declared format.

| offset | size | field                                          |
| ------ | ---- | ---------------------------------------------- |
| 0      | 4    | magic = `b"CDSP"`                              |
| 4      | 1    | version = `1`                                  |
| 5      | 1    | format (0=S16LE, 1=S32LE, 2=F32LE)             |
| 6      | 2    | channels (u16 LE)                              |
| 8      | 4    | sample_rate (u32 LE)                           |
| 12     | 4    | reserved (u32 LE, sender writes zero)          |

No per-frame framing, no checksum, no timestamp. TCP is the integrity
guarantee. See `proto/src/lib.rs` for the encoder/decoder.

## Wire contract with CamillaDSP

The PC sender's `--format / --rate / --channels` MUST match the `capture`
section of whatever `bridge.yml` you're using on the Pi. The defaults shipped
in `deploy/bridge.yml` are:

- `format: S32LE`
- `capture_samplerate: 48000`
- `channels: 2`

These match the pc-sender defaults (`--format s32le --rate 48000
--channels 2`). 48 kHz is the shared-mode mixer rate on most Windows
(WASAPI) and Linux (PipeWire / PulseAudio) systems, so it works
out-of-the-box on the great majority of PCs.

If your PC is configured at 44.1 kHz, 96 kHz or another rate, run:

```sh
pc-sender list-devices    # shows supported rate ranges per device
```

…then either change Sound settings to 48 kHz or override both ends in
lockstep:

1. Edit `deploy/bridge.yml` `capture.format` / `capture_samplerate` /
   `capture.channels`.
2. Pass the matching `--format / --rate / --channels` on `pc-sender`.

CamillaDSP's internal rate stays at 96 kHz; its AsyncSinc resampler upsamples
the wire rate transparently. **There is no PC-side resampling.**

## Running it

### On the Pi (manual, for a smoke test)

```sh
scp deploy/bridge.yml fwmt@hifiberry.local:~/camilladsp/configs/
scp target/aarch64-unknown-linux-gnu/release/pi-receiver fwmt@hifiberry.local:~/

ssh fwmt@hifiberry.local
sudo install -m 0755 ~/pi-receiver /usr/local/bin/pi-receiver

/usr/local/bin/pi-receiver \
  --port 9000 \
  --device hw:Loopback,0,0 \
  --buffer-ms 200 \
  --camilla-host 127.0.0.1 --camilla-port 1234 \
  --bridge-config /home/fwmt/camilladsp/configs/bridge.yml \
  --idle-config   /home/fwmt/camilladsp/configs/music.yml
```

While `pi-receiver` runs, any PC on the LAN can connect on port 9000 and
push PCM. While that PC stays connected, CamillaDSP runs `bridge.yml`. When
it disconnects, CamillaDSP is switched back to `music.yml`.

### On the PC

```sh
# Linux
pc-sender --host hifiberry.local --port 9000

# Windows (PowerShell)
pc-sender.exe --host hifiberry.local --port 9000
```

Useful flags (defaults in parentheses):

- `--device <name>` — device name, case-insensitive. `default` (default)
  picks the host's default input. Use `pc-sender list-devices` to inspect.
- `--cpal-host <name>` — pin a cpal host backend (e.g. `wasapi`, `alsa`).
  Empty = platform default.
- `--rate / --format / --channels` — wire format. Must match `bridge.yml`.
- `--reconnect-ms <ms>` — initial reconnect delay (1000). Doubles up to 5 s
  on consecutive failures.

## Install as a systemd service on the Pi

```sh
# One-time: dedicated unprivileged service user with audio group access.
sudo useradd --system --no-create-home --shell /usr/sbin/nologin -G audio pi-camilla

# Install the binary and configs.
sudo install -m 0755 target/aarch64-unknown-linux-gnu/release/pi-receiver \
  /usr/local/bin/pi-receiver
install -m 0644 deploy/bridge.yml /home/fwmt/camilladsp/configs/bridge.yml
sudo install -m 0644 deploy/pi-receiver.service /etc/systemd/system/

sudo systemctl daemon-reload
sudo systemctl enable --now pi-receiver
journalctl -u pi-receiver -f
```

If you want a different bridge config path, edit
`/etc/systemd/system/pi-receiver.service` `--bridge-config` and reload.

## Troubleshooting

- **No sound at all.** Verify the loopback is exposed and CamillaDSP captures
  from the right side:

  ```sh
  aplay -L | grep -i loopback
  cat /proc/asound/Loopback/pcm0p/sub0/status   # while pc-sender is running
  cat /proc/asound/Loopback/pcm1c/sub0/status
  ```

  `pcm0p` (playback half — what we write to) should be `RUNNING` while the
  PC is connected, owned by `pi-receiver`. `pcm1c` (capture half) should be
  `RUNNING`, owned by `camilladsp` reading our audio.

- **`warn: ALSA writei error → try_recover` repeating.** Underrun. Bump
  `--buffer-ms` (e.g. 300, 500). Check the LAN — wired gigabit absorbs 6 Mbps
  at S32/96k easily, but Wi-Fi with contention is unreliable.

- **`Connection refused`.** `pi-receiver` not running, firewall blocking
  9000, or wrong `--host`. `nc -vz hifiberry.local 9000` from the PC.

- **Sample rate mismatch / silence even though TCP is flowing.** The wire
  format doesn't match `bridge.yml` capture. Run `pc-sender list-devices`,
  confirm what your input device is producing, and align both ends.

- **`config swap failed` warning at session start/end.** The websocket call
  to CamillaDSP failed (wrong host/port, CamillaDSP restarting). Audio
  still flows under whichever config CamillaDSP is currently running. Check
  with `systemctl status camilladsp` and `--camilla-host / --camilla-port`.

- **cpal can't find a "loopback" device on Windows.** Use `pc-sender
  list-devices` and pick the **monitor of your output device** (e.g.
  "Headphones (loopback)" exposed by WASAPI), not a microphone input.
  PipeWire on Linux similarly exposes `monitor of` sources for each output.

- **Tidal Connect stops working while bridge is connected.** Expected — both
  paths feed the same `hw:Loopback,0`. The bridge config is reverted to your
  idle `music.yml` automatically when `pc-sender` disconnects, so Tidal can
  resume after that.

## Layout

```
.
├── mise.toml             # toolchain + tasks
├── Cargo.toml            # workspace root
├── Cross.toml            # cross-compile config (Pi aarch64 + libasound2-dev)
├── deny.toml             # cargo-deny: advisories + license allowlist
├── .config/nextest.toml  # test runner profile
├── proto/                # wire header / format enum / errors
├── pi-receiver/          # daemon on the Pi (TCP + ALSA + WS to CamillaDSP)
├── pc-sender/            # PC client (cpal capture + TCP)
└── deploy/
    ├── bridge.yml            # CamillaDSP config used while bridge is active
    └── pi-receiver.service   # systemd unit
```
