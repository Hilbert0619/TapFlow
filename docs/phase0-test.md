# Phase 0 test: can the iPhone tap in other apps by itself?

Two engines are tested:

- **HID**: Apple's developer HID service, no extra app.
- **XCTest**: a WebDriverAgent runner.

Run this on each iPhone (iOS 18 and iOS 26). Files are in `C:\Users\AI\Downloads\TapFlow\`.

## Once per phone

1. **Install** with iloader: `TapFlowSpike.ipa` ("TapFlow Test") and `WebDriverAgentRunner.ipa`. With CourseTable,
   that's the free account's 3 apps.
2. **Developer Mode**: Settings → Privacy & Security → Developer Mode → on. The iPhone restarts; confirm with
   your passcode.
3. **LocalDevVPN** from the App Store; open it and switch it on. It stays on while you test.
4. **Pairing file** from iloader on the PC (pairing file / Manage pairing file):
   - iOS 18 or iOS 26.0–26.3: choose **Lockdown**. Both engines then work.
   - iOS 26.4 or later: choose **RPPairing**. Only the HID engine works with it for now.

   Send the file to the iPhone (AirDrop, or save to Files via iCloud/USB). In TapFlow Test, tap **Import Pairing
   File…** and pick it.

## The test (TapFlow Test, top to bottom)

1. **Connect** (the address stays 10.7.0.1). Allow Local Network if asked.
   - The info box shows the iOS version and "Developer Mode: true".
   - If it says **Mounted images: 0**, tap **Download & Mount Developer Disk Image**. It downloads 17 MB, then asks
     Apple to sign it.
2. **Services**: the first line is **HID (dtuhidd): true / false**. Note it.
3. **HID engine**: tap **One swipe up**. The form should scroll by itself.
4. **A timed run you can see**:
   - Open the Calculator and type `0 + 1 =`.
   - Back in TapFlow Test, move the Point sliders onto the Calculator's **=** key. A rough guide is x 87 %,
     y 89 %; try 5 taps first and adjust.
   - Set Taps to 600 and Every 1000 ms. Keep "Keep running in the background" on.
   - Tap **Start taps** and switch to the Calculator within 5 s. Each tap adds 1 to the display.
   - Leave it for 10 minutes. Halfway through, lock the iPhone for 30 s, then unlock it.
5. **Speed**: set Every 50 ms and Taps 200, and run on the Calculator again. Does the display reach 200?
6. **XCTest engine** (Lockdown file only):
   - Choose Engine → XCTest (WDA).
   - Tap **Find WebDriverAgent runners**; the runner's ID fills in.
   - Tap **Launch runner** and wait up to a minute for "WDA ready on port 8100".
   - Run step 4 again with this engine (60 taps is enough).
7. **Send**: tap **Copy log** and paste it into the chat. Add the iOS version, a screenshot of the Calculator's
   final number, and what happened when the phone was locked.

## What decides go / no-go

- **Go**: either engine, started from the phone alone, keeps tapping in the Calculator for 10 minutes with
  TapFlow Test in the background.
- The log, the HID line from step 2 and the lock behaviour decide which engine Phase 1 builds on.
