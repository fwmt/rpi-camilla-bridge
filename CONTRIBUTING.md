# Contributing to rpi-camilla-bridge

Thanks for considering a contribution. This is a small, focused project —
PRs are most welcome when they keep it that way.

## Before you open a PR

```sh
mise run ci
```

That chains `fmt-check + clippy -D warnings + nextest + cargo-deny +
cargo-machete`. Same pipeline GitHub Actions runs on your PR. If it's
green locally, the bot will be happy.

For changes that touch `pi-receiver/src/alsa_out.rs` or
`pc-sender/src/audio_in.rs`, please also do a manual smoke test:
`pc-sender` to a real `pi-receiver` for at least 30 s and confirm the
session ends cleanly (graceful Ctrl+C, no `try_recover` warnings unless
you're deliberately stress-testing the underrun path).

## Scope

Things this project deliberately does **not** do, and PRs are unlikely
to be accepted to add:

- Sample-rate conversion in the wire path (CamillaDSP does it, with
  AsyncSinc; doing it again would be redundant and lossy).
- Adaptive clock-drift compensation (CamillaDSP's `enable_rate_adjust`
  handles this on the Pi side).
- Encryption or authentication (LAN-only, by design).
- mDNS / Zeroconf discovery (a `--host` flag is enough for the
  problem we're solving).
- A GUI. CLI only.
- Tokio or any async runtime — the threading model is intentionally
  blocking-IO + a couple of `std::thread`s, and that has been measured
  to be enough.
- Multi-client simultaneous capture on the Pi.

If you have a use case that bends one of these rules and you think it's
worth bending, open an issue first to discuss before sinking time into a
PR.

## Examples

PRs adding `examples/bridge-*.yml` for additional channel layouts
(2.0, 2.1, 5.1, 7.1.4) or DAC families are welcome. Include a header
comment explaining:

- the hardware (DAC, amp, speakers) you tested it against,
- which `pc-sender` flags it pairs with,
- what protections (limiters, crossovers, gain) are in the pipeline,
- what's deliberately omitted and why.

Real-world configs are educational. **Never copy gain or limiter values
between hardware setups verbatim** — speaker thermal limits and amplifier
sensitivities don't transfer.

## Bug reports

Run the daemons with `RUST_LOG=debug` and capture logs from both ends.
Include in the issue:

1. Topology — output of `cat /proc/asound/cards` on the Pi and
   `pc-sender list-devices` on the PC.
2. Your bridge config's `capture` section (the rest can be omitted).
3. Wire flags: `pc-sender --rate / --format / --channels`.
4. Log excerpts from both `journalctl -u pi-receiver -b` and the
   `pc-sender` stderr.
5. Steps to reproduce and what you expected vs what happened.

A working repro that fits in a single `pc-sender` invocation is gold.

## Code style

- `rustfmt` defaults (the workspace doesn't override).
- Comments on **why**, not what — well-named identifiers do the latter.
- Don't add error handling for impossible scenarios. Trust framework
  guarantees and validate at the boundaries (network, CLI, ALSA).
- No half-finished implementations on `main`. If a change is large,
  break it into independently-reviewable PRs.

## Licensing

By submitting a PR you agree to license your contribution under both
MIT and Apache-2.0, matching the repository's dual license. There is no
CLA.
