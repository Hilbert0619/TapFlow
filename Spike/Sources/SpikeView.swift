import SwiftUI
import UniformTypeIdentifiers

@main
struct TapFlowSpikeApp: App {
    init() { Engine.installLog() }

    var body: some Scene {
        WindowGroup { SpikeView() }
    }
}

/// Phase 0: can this iPhone tap in other apps by itself? Connect through LocalDevVPN with a pairing file, then try
/// the HID engine and the XCTest (WebDriverAgent) engine with a timed run of taps.
struct SpikeView: View {
    enum EngineKind: String, CaseIterable, Identifiable {
        case hid = "HID"
        case wda = "XCTest (WDA)"
        var id: String { rawValue }
    }

    @ObservedObject private var log = Log.shared
    @AppStorage("host") private var host = "10.7.0.1"
    @AppStorage("runner") private var runner = ""
    @AppStorage("rpPort") private var rpPort = 49152
    @State private var hasPairingFile = FileManager.default.fileExists(atPath: SpikeView.pairingURL.path)
    @State private var importing = false
    @State private var connected = false
    @State private var info = ""
    @State private var busy = false
    @State private var runners: [String] = []
    @State private var wdaPort: Int?

    @State private var engine = EngineKind.hid
    @State private var x = 0.5
    @State private var y = 0.5
    @State private var delay = 5.0
    @State private var count = 600
    @State private var intervalMs = 1000
    @State private var keepAlive = true
    @State private var run: Task<Void, Never>?
    @State private var done = 0
    @State private var failed = 0

    static var pairingURL: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("pairing.plist")
    }

    var body: some View {
        NavigationStack {
            Form {
                setupSection
                if connected { engineSection; runSection }
                logSection
            }
            .navigationTitle("TapFlow Test")
            .fileImporter(isPresented: $importing, allowedContentTypes: [.item]) { result in
                importPairingFile(result)
            }
        }
    }

    // MARK: - Setup

    private var setupSection: some View {
        Section {
            Button(hasPairingFile ? "Pairing file ✓ (replace…)" : "Import Pairing File…") { importing = true }
            TextField("Device address", text: $host)
                .keyboardType(.decimalPad)
            LabeledContent("RPPairing port") {
                TextField("49152", value: $rpPort, format: .number.grouping(.never))
                    .keyboardType(.numberPad)
                    .multilineTextAlignment(.trailing)
            }
            Button(connected ? "Reconnect" : "Connect") { perform(connect) }
                .disabled(!hasPairingFile || busy)
            if connected {
                Button("Device & Developer Mode") { perform { info = try await Engine.deviceStatus() } }
                Button("Services") { perform { info = try await Engine.services() } }
                Button("Download & Mount Developer Disk Image") { perform(mountDDI) }
            }
            if !info.isEmpty {
                Text(info).font(.caption.monospaced()).textSelection(.enabled)
            }
        } header: {
            Text("1. Connect")
        } footer: {
            Text("Turn on LocalDevVPN first. The pairing file comes from iloader on your PC (Manage Pairing File → Export); it holds both pairings and the app tries RPPairing first, then Lockdown.")
        }
    }

    // MARK: - Engine

    private var engineSection: some View {
        Section("2. Engine") {
            Picker("Engine", selection: $engine) {
                ForEach(EngineKind.allCases) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented)
            if engine == .wda {
                Button("Find WebDriverAgent runners") {
                    perform {
                        runners = try await Engine.findRunners()
                        if runner.isEmpty, let first = runners.first { runner = first }
                        info = runners.isEmpty ? "No *xctrunner app installed." : runners.joined(separator: "\n")
                    }
                }
                TextField("Runner bundle ID", text: $runner)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                Button(wdaPort == nil ? "Launch runner" : "Relaunch runner (port \(wdaPort!))") {
                    perform { wdaPort = try await Engine.startWDA(bundleID: runner) }
                }
                .disabled(runner.isEmpty || busy)
            } else {
                Button("One swipe up (from 80 % to 30 % height)") {
                    perform {
                        try await Engine.hidBegin()
                        try await Engine.hidDrag(from: (0.5, 0.8), to: (0.5, 0.3))
                        try await Engine.hidEnd()
                    }
                }
            }
        }
    }

    // MARK: - Timed run

    private var runSection: some View {
        Section {
            LabeledContent("Point", value: String(format: "%.0f %% × %.0f %%", x * 100, y * 100))
            Slider(value: $x, in: 0...1) { Text("x") }
            Slider(value: $y, in: 0...1) { Text("y") }
            Stepper("Start after \(Int(delay)) s", value: $delay, in: 0...60, step: 1)
            Stepper("Taps: \(count)", value: $count, in: 1...100_000, step: count < 100 ? 10 : 100)
            Stepper("Every \(intervalMs) ms", value: $intervalMs, in: 20...10_000, step: intervalMs <= 100 ? 10 : 100)
            Toggle("Keep running in the background", isOn: $keepAlive)
            if run == nil {
                Button("Start taps") { startRun() }
                    .disabled(busy || (engine == .wda && wdaPort == nil))
            } else {
                Button("Stop", role: .destructive) { run?.cancel() }
                LabeledContent("Done", value: "\(done) / \(count), failed \(failed)")
            }
        } header: {
            Text("3. Timed run")
        } footer: {
            Text("Start, then switch to another app within the delay. Each tap lands at the point; the log keeps the timing.")
        }
    }

    private var logSection: some View {
        Section {
            Button("Copy log") { UIPasteboard.general.string = log.lines.joined(separator: "\n") }
            ForEach(Array(log.lines.suffix(80).enumerated().reversed()), id: \.offset) { _, line in
                Text(line).font(.caption2.monospaced()).textSelection(.enabled)
            }
        } header: {
            Text("Log")
        }
    }

    // MARK: - Actions

    private func perform(_ action: @escaping () async throws -> Void) {
        busy = true
        Task {
            do { try await action() } catch { log.add("✗ \(error.localizedDescription)") }
            busy = false
        }
    }

    private func importPairingFile(_ result: Result<URL, Error>) {
        do {
            let url = try result.get()
            let access = url.startAccessingSecurityScopedResource()
            defer { if access { url.stopAccessingSecurityScopedResource() } }
            let data = try Data(contentsOf: url)
            try data.write(to: Self.pairingURL, options: .atomic)
            hasPairingFile = true
            log.add("pairing file imported (\(data.count) bytes)")
        } catch {
            log.add("✗ import: \(error.localizedDescription)")
        }
    }

    private func connect() async throws {
        connected = false
        wdaPort = nil
        try await Engine.connect(pairingFile: Self.pairingURL, host: host, rpPort: UInt16(clamping: rpPort))
        connected = true
        info = try await Engine.deviceStatus()
    }

    private func mountDDI() async throws {
        let files = try await DeveloperDiskImage.ensure()
        try await Engine.mountDDI(image: files.image, manifest: files.manifest, trustCache: files.trustCache)
        info = try await Engine.deviceStatus()
    }

    private func startRun() {
        done = 0
        failed = 0
        if keepAlive { KeepAlive.shared.start() }
        let (engine, x, y, delay, count, interval) = (engine, x, y, delay, count, intervalMs)
        let port = wdaPort
        log.add("run: \(engine.rawValue), \(count) taps every \(interval) ms at \(Int(x * 100))%×\(Int(y * 100))%, start in \(Int(delay)) s")
        run = Task {
            let started = Date()
            do {
                var wda: (client: WDAClient, session: String, size: CGSize)?
                if engine == .hid {
                    try await Engine.hidBegin()
                } else if let port {
                    let client = WDAClient(port: port)
                    let session = try await client.newSession()
                    wda = (client, session, try await client.windowSize(session: session))
                    log.add("WDA session \(session), screen \(Int(wda!.size.width))×\(Int(wda!.size.height)) pt")
                }
                try await Task.sleep(for: .seconds(delay))
                let firstTap = Date()
                for index in 1...count {
                    try Task.checkCancellation()
                    let tapStart = Date()
                    do {
                        if let wda {
                            try await wda.client.tap(session: wda.session, x: x * wda.size.width, y: y * wda.size.height)
                        } else {
                            try await Engine.hidTap(x: x, y: y)
                        }
                        done += 1
                    } catch {
                        failed += 1
                        log.add("tap \(index) failed: \(error.localizedDescription)")
                        if failed >= 5 && done == 0 { throw error }
                    }
                    if index == 1 || index % 50 == 0 {
                        log.add(String(format: "tap %d took %.0f ms; %.0f s since the first", index,
                                       Date().timeIntervalSince(tapStart) * 1000, Date().timeIntervalSince(firstTap)))
                    }
                    let wait = Double(interval) / 1000 - Date().timeIntervalSince(tapStart)
                    if wait > 0 { try await Task.sleep(for: .seconds(wait)) }
                }
            } catch is CancellationError {
                log.add("stopped")
            } catch {
                log.add("✗ run: \(error.localizedDescription)")
            }
            if engine == .hid { try? await Engine.hidEnd() }
            log.add(String(format: "run over: %d done, %d failed, %.0f s", done, failed, Date().timeIntervalSince(started)))
            if keepAlive { KeepAlive.shared.stop() }
            run = nil
        }
    }
}
