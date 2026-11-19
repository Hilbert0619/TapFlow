# Session Handoff — TapFlow (Phase 0 test app built — waiting for the owner's phone test)

Date: 2026-11-19 · Repo: `D:\Projects\TapFlow` → https://github.com/Hilbert0619/TapFlow (public), branch `main`

## What and why

A separate app (not CourseTable) to record gestures and replay them, and to tap a point once or continuously in
other apps. It must work **without jailbreak or TrollStore**, on iOS 18 and 26 (27 if possible). The reference,
mango6i/iOS-AutoTap, needs TrollStore and has no source.

The plan, approved by the owner, is in `C:\Users\AI\.claude\plans\add-a-new-feature-snug-hummingbird.md`.

**Owner's choices (2026-11-19):**
- several iOS versions;
- the no-PC route, with one-time setup accepted (Developer Mode, a pairing file from iloader, LocalDevVPN);
- a new separate app;
- a public repo.

## Phase 0 (now): the go/no-go test

- **Route:** the app talks to the iPhone's *own* developer services through LocalDevVPN (10.7.0.1) and a pairing
  file, as StikDebug does, with [`idevice`](https://github.com/jkcoxson/idevice) 0.1.68 (MIT).
- **Two engines:**
  1. **HID**: CoreDevice's HID service (`dtuhidd`, in the Xcode 27 developer disk image), the one Device Hub uses.
     No runner app. Touches are dropped unless a displayservice media stream is open; its RTP is drained.
     Confirmed by idevice on iOS 27.0; iOS 18 and 26 are unknown.
  2. **XCTest**: Appium WebDriverAgent v16.15.3 (`com.facebook.WebDriverAgentRunner.xctrunner`, renamed by iloader),
     launched with idevice `XCUITestService::run_until_wda_ready`. The app reaches it at 127.0.0.1:8100.
- **Pairing files:**
  - **Lockdown**: `TcpProvider`, heartbeat, `CoreDeviceProxy` tunnel. Both engines.
  - **RPPairing** (needed on iOS 26.4+, StikDebug's path): `RemotePairingClient` on port 49152, then a TLS-PSK
    tunnel. HID only, because idevice's XCTest service needs a lockdown provider.
- **Code:**
  - `engine/` is a Rust staticlib with a C API (`engine/include/tapflow_engine.h`).
  - `Spike/` is a SwiftUI one-screen app, built with XcodeGen on CI.
  - `.github/workflows/spike.yml` runs a Linux `cargo check`, then builds the app IPA and the WDA runner IPA on
    macos-26.
- **CI:** run 44571267162, all green. IPAs: `C:\Users\AI\Downloads\TapFlow\TapFlowSpike.ipa` and
  `WebDriverAgentRunner.ipa`.
- **Owner's checklist:** `docs/phase0-test.md`.

**Not built yet (later phases):**
- TapKit (logic plus tests on Windows);
- the real app (pick the target app, screenshot, place a point, record a gesture, scripts, Live Activity
  controls).

## Results so far

- **2026-11-19, iOS 27.0, first build:** the build used the Lockdown half of iloader's combined pairing file, and
  the heartbeat failed with "Broken pipe": iOS 26.4+ refuses lockdown over the network.
- **Fix** (run 44581797843): RPPairing first, then Lockdown.
- The owner's phones: **iOS 18.3 and 27.0**. Only TapFlow Test is installed so far, not the WDA runner.

## Next

Wait for the owner's Phase 0 results (the log, the iOS versions, the HID true/false line, and what happened on
lock).

- **HID works:** Phase 1 builds on it. Recorded gestures become raw touchscreen reports
  (`UniversalHidServiceClient::send_touchscreen`).
- **Only XCTest works:**
  - For iOS 26.4+ with RPPairing, XCTest launch over an RSD-only tunnel is needed (idevice's `XCUITestService`
    opens its own `CoreDeviceProxy`): fork or extend it.
- **Neither works:** report it, and offer PC-tethered mode or the AssistiveTouch/Switch Control guide.

Also worth a look in Phase 1: StikDebug's **on-device pairing** (idevice `pairable_host_*`). The app advertises
itself as a Mac and the iPhone pairs with it by PIN, which may remove the PC step entirely. See
StikDebug/Services/OnDevicePairingService.swift.

## Facts and gotchas

- Free Apple account: 3 apps. CourseTable, TapFlow Test and the WDA runner fill them.
- The DDI mirror `doronz88/DeveloperDiskImage` is at Xcode 27 (27B52, updated 2026-11-11). Mounting asks Apple's
  TSS server to sign it.
- GitHub's API refuses anonymous macOS runners now and then: find tags with `git ls-remote`.
- No Rust on this PC (no global installs): CI compiles. The Linux `cargo check` job catches errors in about 3 min.
- Commit identity as CourseTable (per repo). GitHub needs Clash: `git -c http.proxy=http://127.0.0.1:7892`,
  `HTTPS_PROXY` for gh.

## How I like to work

- Communicate in English. I give goals; do as much as you can and keep my manual steps minimal.
- Short report at the end of each stage, with run links. Verify for real (tests, CI, screenshots), don't trust
  "should work".
- Don't bypass security mechanisms; if something needs my approval or sign-in, say exactly what to do.
- "Complete the handover" means: finish the docs so a new session starts the next thing; don't start it.

## Paused 2026-11-19: direction changed to PC-tethered

- **On the phone alone it failed on iOS 27.0:** RPPairing reached port 49152, but pair-verify was reset by the
  peer; Lockdown was refused (heartbeat Broken pipe). Not investigated further. Possible causes: the
  remote-pairing port (StikDebug finds it with Bonjour) or iOS 27 changes.
- **The owner's choice:** keep the iPhone plugged into the PC, with **controls on the PC**. A small TapFlow program
  on Windows serves a local browser page: the phone's screen, click to place taps, drag to record gestures,
  scripts, start/stop. Nothing extra on the iPhone.
- **USB test so far** (official idevice-tools-windows-v0.1.68, SHA-256 checked, in the session scratchpad; the
  DDI from the doronz88 mirror):
  - Both phones are seen over usbmux: iPhone 15 Pro on iOS 27.0, iPhone 12 on iOS 18.3.
  - DDI mounted on the 15 Pro; the 12 refused while locked.
  - DVT screenshots work.
  - One `hid button home` was accepted (the media-stream gate opened) but its effect wasn't confirmed.
- **Resume with:**
  1. **Ask the owner first** (they want to be asked before any access to their phones, every time).
  2. With both phones unlocked on the Home Screen: press Home, swipe between pages, tap empty wallpaper.
     Prefer checks that don't capture screen content; delete any screenshots afterwards.
  3. Then plan and build the PC app: a Rust binary on idevice over usbmux, with the HID engine, or WDA if HID
     fails on 18.3. Build it on CI windows-latest, with logic tests.
