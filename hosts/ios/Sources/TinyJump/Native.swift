import UIKit

/// Object storage: one JSON file per key, written atomically.
final class FileStore {
    private let dir: URL

    init(dir: URL) {
        self.dir = dir
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    }

    /// Percent-encodes everything but [a-z0-9._-], so keys never become paths, and upper-case
    /// letters too, so keys differing only in case stay apart (APFS ignores case).
    static func encode(_ key: String) -> String {
        var out = ""
        for b in key.utf8 {
            let safe = (b >= 0x30 && b <= 0x39) || (b >= 0x61 && b <= 0x7A) || b == 0x2D || b == 0x5F || b == 0x2E
            out += safe ? String(UnicodeScalar(b)) : String(format: "%%%02X", Int(b))
        }
        return out
    }

    private func url(_ key: String) -> URL {
        dir.appendingPathComponent(FileStore.encode(key) + ".json")
    }

    func load(_ key: String) -> Data? {
        try? Data(contentsOf: url(key))
    }

    func save(_ key: String, _ value: Data) -> Bool {
        do {
            try value.write(to: url(key), options: .atomic)
            return true
        } catch {
            return false
        }
    }

    func keys(_ prefix: String) -> [String] {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? []
        return names
            .filter { $0.hasSuffix(".json") }
            .compactMap { String($0.dropLast(5)).removingPercentEncoding }
            .filter { $0.hasPrefix(prefix) }
            .sorted()
    }
}

/// What the Rust runtime calls back into: storage and widget measurement.
final class HostBridge {
    let store: FileStore
    weak var renderer: Renderer?

    init(store: FileStore) {
        self.store = store
    }
}

/// The Rust runtime behind the C ABI (crates/tj-ffi/include/tinyjump.h). Main thread only.
final class NativeRuntime {
    private let bridge: HostBridge
    private var rt: OpaquePointer?

    init(bridge: HostBridge) {
        self.bridge = bridge
        var s = TjHost()
        s.ctx = Unmanaged.passUnretained(bridge).toOpaque()
        s.load = { ctx, key, keyLen, out in
            let store = Unmanaged<HostBridge>.fromOpaque(ctx!).takeUnretainedValue().store
            guard let data = store.load(NativeRuntime.string(key, keyLen)) else { return 0 }
            data.withUnsafeBytes { raw in
                tj_buf_set(out, raw.bindMemory(to: UInt8.self).baseAddress, raw.count)
            }
            return 1
        }
        s.save = { ctx, key, keyLen, value, valueLen in
            let store = Unmanaged<HostBridge>.fromOpaque(ctx!).takeUnretainedValue().store
            let data = value.map { Data(bytes: $0, count: valueLen) } ?? Data()
            return store.save(NativeRuntime.string(key, keyLen), data) ? 0 : 1
        }
        s.keys = { ctx, prefix, prefixLen, out in
            let store = Unmanaged<HostBridge>.fromOpaque(ctx!).takeUnretainedValue().store
            let keys = store.keys(NativeRuntime.string(prefix, prefixLen))
            let json = (try? JSONSerialization.data(withJSONObject: keys)) ?? Data("[]".utf8)
            json.withUnsafeBytes { raw in
                tj_buf_set(out, raw.bindMemory(to: UInt8.self).baseAddress, raw.count)
            }
        }
        s.measure = { ctx, node, maxWidth, width, height in
            let bridge = Unmanaged<HostBridge>.fromOpaque(ctx!).takeUnretainedValue()
            let size = bridge.renderer?.measure(node: Int(node), maxWidth: maxWidth < 0 ? nil : CGFloat(maxWidth)) ?? .zero
            width?.pointee = Float(size.width)
            height?.pointee = Float(size.height)
        }
        rt = tj_new(s)
    }

    deinit {
        tj_free(rt)
    }

    static func string(_ p: UnsafePointer<UInt8>?, _ n: Int) -> String {
        guard let p = p, n > 0 else { return "" }
        return String(decoding: UnsafeBufferPointer(start: p, count: n), as: UTF8.self)
    }

    /// Compiles and boots; status codes as in tj_core::host, with the JSON payload.
    func boot(source: String, now: Double, seed: UInt64) -> (Int32, String) {
        let bytes = Array(source.utf8)
        let code = bytes.withUnsafeBufferPointer { tj_boot(rt, $0.baseAddress, $0.count, now, seed, 1) }
        return (code, out())
    }

    func handle(now: Double, input: String) -> (Int32, String) {
        let bytes = Array(input.utf8)
        let code = bytes.withUnsafeBufferPointer { tj_handle(rt, now, $0.baseAddress, $0.count) }
        return (code, out())
    }

    private func out() -> String {
        var len = 0
        guard let p = tj_out(rt, &len) else { return "" }
        return NativeRuntime.string(p, len)
    }
}
