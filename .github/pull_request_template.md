<!-- Keep this short. CI runs `mise run ci` automatically. -->

## What

<!-- One paragraph. What does this change and why. -->

## Test plan

- [ ] `mise run ci` is green locally.
- [ ] If touching `pi-receiver/src/alsa_out.rs` or
      `pc-sender/src/audio_in.rs`: smoke-tested at least 30 s of audio
      end-to-end and confirmed Ctrl+C exits cleanly.
- [ ] If touching the wire protocol or `proto/`: bumped `VERSION` and
      added a roundtrip test.
- [ ] If adding an `examples/bridge-*.yml`: header documents hardware,
      pc-sender flags, included protections, and what's deliberately
      omitted.

## Notes for reviewer

<!-- Anything non-obvious: alternatives considered, follow-ups deferred,
     hardware you tested against, etc. -->
