# TapFlow

Record gestures on an iPhone and replay them, and tap a point once or continuously, **without jailbreak or
TrollStore** (iOS 17.4+, aiming at iOS 18 and 26).

An ordinary iOS app can't touch other apps. TapFlow uses the iPhone's own developer services instead. It reaches
them through a loopback VPN (LocalDevVPN) and a pairing file, the same way StikDebug does, using the
[`idevice`](https://github.com/jkcoxson/idevice) library (MIT). It then taps in one of two ways:

- **HID**: CoreDevice's HID service in the developer disk image (the one Xcode's Device Hub uses). No extra app.
- **XCTest**: a WebDriverAgent runner launched through testmanagerd.

## Status: Phase 0 (feasibility test)

`Spike/` is a one-screen test app and `engine/` its Rust engine. The test checks whether the phone can start
either engine by itself and keep tapping in another app for 10 minutes. CI (`.github/workflows/spike.yml`) builds
`TapFlowSpike.ipa` and `WebDriverAgentRunner.ipa` for iloader.

Requirements on the iPhone:

- Developer Mode on.
- A pairing file from iloader.
- LocalDevVPN on.

## Licence

MIT. `engine/src/stream.rs` is adapted from idevice-tools (MIT, Jackson Coxson).
