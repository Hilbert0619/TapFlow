import Foundation

/// The Rust engine's calls, run one at a time off the main thread.
enum Engine {
    private static let queue = DispatchQueue(label: "TapFlow.engine")

    struct Failure: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    static func installLog() {
        tf_set_log { line in
            guard let line else { return }
            let text = String(cString: line)
            Task { @MainActor in Log.shared.add(text) }
        }
    }

    /// Runs `call`, which returns an error message or nil.
    static func run(_ call: @escaping () -> UnsafeMutablePointer<CChar>?) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            queue.async {
                if let error = call() {
                    let message = String(cString: error)
                    tf_free(error)
                    continuation.resume(throwing: Failure(message: message))
                } else {
                    continuation.resume()
                }
            }
        }
    }

    /// Runs `call`, which fills a string.
    static func text(_ call: @escaping (UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> UnsafeMutablePointer<CChar>?) async throws -> String {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<String, Error>) in
            queue.async {
                var out: UnsafeMutablePointer<CChar>?
                if let error = call(&out) {
                    let message = String(cString: error)
                    tf_free(error)
                    continuation.resume(throwing: Failure(message: message))
                } else {
                    let text = out.map { String(cString: $0) } ?? ""
                    tf_free(out)
                    continuation.resume(returning: text)
                }
            }
        }
    }

    /// `rpPort` is used with an RPPairing file only.
    static func connect(pairingFile: URL, host: String, rpPort: UInt16) async throws {
        try await run { tf_connect(pairingFile.path, host, rpPort) }
    }

    static func services() async throws -> String { try await text { tf_services($0) } }
    static func deviceStatus() async throws -> String { try await text { tf_device_status($0) } }

    static func mountDDI(image: URL, manifest: URL, trustCache: URL) async throws {
        try await run { tf_mount_ddi(image.path, manifest.path, trustCache.path) }
    }

    static func hidBegin() async throws { try await run { tf_hid_begin() } }
    static func hidEnd() async throws { try await run { tf_hid_end() } }

    /// `x`, `y` from 0 to 1 across the screen.
    static func hidTap(x: Double, y: Double) async throws {
        let (ux, uy) = (units(x), units(y))
        try await run { tf_hid_tap(ux, uy) }
    }

    static func hidDrag(from: (Double, Double), to: (Double, Double), steps: UInt32 = 12, delayMs: UInt64 = 16) async throws {
        let (x1, y1, x2, y2) = (units(from.0), units(from.1), units(to.0), units(to.1))
        try await run { tf_hid_drag(x1, y1, x2, y2, steps, delayMs) }
    }

    static func findRunners() async throws -> [String] {
        try await text { tf_find_runners($0) }.split(separator: "\n").map(String.init)
    }

    /// Launches the runner; returns WebDriverAgent's port on this iPhone.
    static func startWDA(bundleID: String) async throws -> Int {
        Int(try await text { tf_wda_start(bundleID, $0) }) ?? 8100
    }

    static func stopWDA() async { await withCheckedContinuation { c in queue.async { tf_wda_stop(); c.resume() } } }

    private static func units(_ fraction: Double) -> UInt16 {
        UInt16((min(max(fraction, 0), 1) * 65535).rounded())
    }
}

/// What the engine and the tests say, newest last.
@MainActor
final class Log: ObservableObject {
    static let shared = Log()
    @Published private(set) var lines: [String] = []
    private let start = Date()

    func add(_ text: String) {
        lines.append(String(format: "%6.1f  ", Date().timeIntervalSince(start)) + text)
        if lines.count > 400 { lines.removeFirst(lines.count - 400) }
    }
}
