# deploy/

What goes onto the Raspberry Pi.

| File                     | Purpose                                                                 |
| ------------------------ | ----------------------------------------------------------------------- |
| `bridge.yml.example`     | Template CamillaDSP config used **while** a PC source is connected.    |
| `pi-receiver.service`    | systemd unit for the daemon.                                            |
| `README.md`              | This file.                                                              |

## What you need to provide

`rpi-camilla-bridge` switches CamillaDSP between two configs over its
websocket:

1. **bridge config** — used while a PC source is streaming. Capture section
   must match the wire format (default `S32LE / 48 kHz / 2ch`), the rest of
   the pipeline (filters, mixers, limiters, playback) is yours.
2. **idle config** — restored when the PC disconnects. This is just your
   normal CamillaDSP config — typically the one you were running before
   installing the bridge (Tidal Connect, Roon, mpd, etc.).

You install both with paths the systemd unit can read.

## Recommended layout on the Pi

```
/etc/rpi-camilla-bridge/bridge.yml      ← copy of bridge.yml.example, adapted
/etc/rpi-camilla-bridge/idle.yml        ← symlink to your existing config,
                                          OR a copy if you don't want surprises
                                          when CamillaDSP UI edits the original
```

Why `/etc/rpi-camilla-bridge/` and not `~/camilladsp/configs/`? Because the
service runs as a dedicated unprivileged user (`pi-camilla`) that has no
home directory; `/etc/` is readable by everyone and survives user deletes.

## Installation

```sh
# 1. Adapt the bridge config to your hardware.
cp deploy/bridge.yml.example /tmp/bridge.yml
$EDITOR /tmp/bridge.yml          # set playback device, channels, filters

# 2. Lay it down on the Pi.
ssh <user>@<pi-host> 'sudo install -d /etc/rpi-camilla-bridge'
scp /tmp/bridge.yml <user>@<pi-host>:/tmp/bridge.yml
ssh <user>@<pi-host> 'sudo install -m 0644 /tmp/bridge.yml /etc/rpi-camilla-bridge/'

# 3. Point idle.yml at your existing CamillaDSP config (read-only symlink).
ssh <user>@<pi-host> 'sudo ln -sf /home/<user>/camilladsp/configs/music.yml /etc/rpi-camilla-bridge/idle.yml'

# 4. Create the service user, install binary + unit, start.
ssh <user>@<pi-host>
sudo useradd --system --no-create-home --shell /usr/sbin/nologin -G audio pi-camilla
sudo install -m 0755 ~/pi-receiver /usr/local/bin/pi-receiver
sudo install -m 0644 deploy/pi-receiver.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now pi-receiver
journalctl -u pi-receiver -f
```

## Why the .example suffix?

Your real `bridge.yml` and `idle.yml` should never be in the repo — gain
staging is hardware-specific and committing it spreads bad defaults. The
top-level `.gitignore` blocks `deploy/bridge.yml` and `deploy/idle.yml`
from being staged accidentally.
