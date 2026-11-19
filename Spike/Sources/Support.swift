import AVFoundation
import Foundation

/// Keeps the app running in the background (it holds the engine's connection) by playing silence, mixed with other
/// audio.
@MainActor
final class KeepAlive {
    static let shared = KeepAlive()
    private var player: AVAudioPlayer?

    var isOn: Bool { player?.isPlaying == true }

    func start() {
        guard player == nil else { return }
        do {
            try AVAudioSession.sharedInstance().setCategory(.playback, options: [.mixWithOthers])
            try AVAudioSession.sharedInstance().setActive(true)
            let player = try AVAudioPlayer(data: Self.silentWAV(seconds: 2))
            player.numberOfLoops = -1
            player.volume = 0
            player.play()
            self.player = player
            Log.shared.add("keep-alive on (silent audio)")
        } catch {
            Log.shared.add("keep-alive failed: \(error.localizedDescription)")
        }
    }

    func stop() {
        player?.stop()
        player = nil
        try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
        Log.shared.add("keep-alive off")
    }

    /// 16-bit mono PCM at 8 kHz, all zeros.
    private static func silentWAV(seconds: Int) -> Data {
        let rate = 8000, samples = rate * seconds, dataSize = samples * 2
        var data = Data()
        func append<T: FixedWidthInteger>(_ value: T) { withUnsafeBytes(of: value.littleEndian) { data.append(contentsOf: $0) } }
        data.append(contentsOf: Array("RIFF".utf8)); append(UInt32(36 + dataSize))
        data.append(contentsOf: Array("WAVEfmt ".utf8)); append(UInt32(16)); append(UInt16(1)); append(UInt16(1))
        append(UInt32(rate)); append(UInt32(rate * 2)); append(UInt16(2)); append(UInt16(16))
        data.append(contentsOf: Array("data".utf8)); append(UInt32(dataSize))
        data.append(Data(count: dataSize))
        return data
    }
}

/// The personalized developer disk image (Xcode's iOS DDI), from the doronz88/DeveloperDiskImage mirror, as
/// StikDebug does. Kept in Caches.
enum DeveloperDiskImage {
    static let base = URL(string: "https://raw.githubusercontent.com/doronz88/DeveloperDiskImage/main/PersonalizedImages/Xcode_iOS_DDI_Personalized/")!
    static let files = ["Image.dmg", "BuildManifest.plist", "Image.dmg.trustcache"]

    static var folder: URL {
        FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0].appendingPathComponent("DDI", isDirectory: true)
    }

    /// The three files, downloaded if missing.
    static func ensure() async throws -> (image: URL, manifest: URL, trustCache: URL) {
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        for name in files {
            let target = folder.appendingPathComponent(name)
            if FileManager.default.fileExists(atPath: target.path) { continue }
            await MainActor.run { Log.shared.add("downloading \(name)…") }
            let (temp, response) = try await URLSession.shared.download(from: base.appendingPathComponent(name))
            guard (response as? HTTPURLResponse)?.statusCode == 200 else {
                throw Engine.Failure(message: "download of \(name) failed")
            }
            try FileManager.default.moveItem(at: temp, to: target)
        }
        return (folder.appendingPathComponent(files[0]), folder.appendingPathComponent(files[1]), folder.appendingPathComponent(files[2]))
    }
}

/// The few WebDriverAgent calls the test needs, on this iPhone's loopback.
struct WDAClient {
    let port: Int
    private var base: URL { URL(string: "http://127.0.0.1:\(port)")! }

    private func call(_ method: String, _ path: String, _ body: [String: Any]? = nil) async throws -> [String: Any] {
        var request = URLRequest(url: base.appendingPathComponent(path))
        request.httpMethod = method
        request.timeoutInterval = 30
        if let body {
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let (data, _) = try await URLSession.shared.data(for: request)
        return (try JSONSerialization.jsonObject(with: data) as? [String: Any]) ?? [:]
    }

    /// A session without launching any app.
    func newSession() async throws -> String {
        let reply = try await call("POST", "session", ["capabilities": ["alwaysMatch": [String: Any]()]])
        if let id = reply["sessionId"] as? String { return id }
        if let value = reply["value"] as? [String: Any], let id = value["sessionId"] as? String { return id }
        throw Engine.Failure(message: "WDA gave no session: \(reply)")
    }

    func windowSize(session: String) async throws -> CGSize {
        let reply = try await call("GET", "session/\(session)/window/size")
        let value = reply["value"] as? [String: Any] ?? [:]
        return CGSize(width: value["width"] as? Double ?? 390, height: value["height"] as? Double ?? 844)
    }

    func tap(session: String, x: Double, y: Double) async throws {
        _ = try await call("POST", "session/\(session)/wda/tap", ["x": x, "y": y])
    }
}
