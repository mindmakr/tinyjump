import Foundation

/// Server objects: calls `@server` methods over HTTP and keeps this device's replicas current
/// from the server's realtime stream. The stream resumes from a change number, so after a
/// reconnect the server sends only what this device missed. Main thread only.
final class Sync: NSObject, URLSessionDataDelegate {
    private weak var tj: TinyJump?
    /// Base URL of a `tj serve` backend, or nil when the app has none.
    private let server: String?
    /// Object key -> change number of the state this device has.
    private var replicas: [String: UInt64] = [:]
    /// Where the stream resumes: the last change number it delivered.
    private var cursor: UInt64 = 0
    private var session: URLSession!
    private var stream: URLSessionDataTask?
    private var buffer = Data()
    private var generation = 0

    init(tj: TinyJump, server: String?) {
        self.tj = tj
        self.server = server.map { $0.hasSuffix("/") ? String($0.dropLast()) : $0 }
        super.init()
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 60 // the server sends a keep-alive every 15s
        session = URLSession(configuration: config, delegate: self, delegateQueue: OperationQueue.main)
    }

    /// Starts keeping `key` current; `seq` is the change number of its cached state.
    func subscribe(_ key: String, seq: UInt64) {
        guard server != nil, replicas[key] == nil else { return }
        replicas[key] = seq
        cursor = replicas.values.min() ?? 0
        connect()
    }

    func stop() {
        generation += 1
        stream?.cancel()
        session.invalidateAndCancel()
    }

    /// Calls a method of an `@server` object; `done` gets its result or an error.
    func call(_ effect: [String: Any], done: @escaping TinyJump.Reply) {
        let object = effect["object"] as? String ?? ""
        guard server != nil, let url = URL(string: objectUrl(object) + "/" + Sync.encode(effect["method"] as? String ?? "")) else {
            done(nil, "no server configured for \(object)")
            return
        }
        var req = URLRequest(url: url, timeoutInterval: 30)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = try? JSONSerialization.data(withJSONObject: effect["args"] as? [Any] ?? [])
        URLSession.shared.dataTask(with: req) { data, _, error in
            let reply = data.flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String: Any]
            DispatchQueue.main.async {
                if let reply = reply, reply["error"] == nil {
                    done(reply["ok"], nil)
                } else {
                    done(nil, reply?["error"] as? String ?? error?.localizedDescription ?? "server call failed")
                }
            }
        }.resume()
    }

    private static func encode(_ s: String) -> String {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        return s.addingPercentEncoding(withAllowedCharacters: allowed) ?? s
    }

    private func objectUrl(_ key: String) -> String {
        (server ?? "") + "/api/" + key.split(separator: "/").map { Sync.encode(String($0)) }.joined(separator: "/")
    }

    private func deliver(_ key: String, _ state: Any, _ seq: UInt64) {
        cursor = max(cursor, seq) // the stream is in change order
        guard let have = replicas[key] else { return }
        replicas[key] = max(have, seq)
        tj?.send(["type": "sync", "object": key, "state": state, "seq": seq])
    }

    /// One stream for all replicas, replaced when the set grows, resuming from `cursor`.
    private func connect() {
        generation += 1
        stream?.cancel()
        buffer = Data()
        let query = replicas.keys.map { "object=" + Sync.encode($0) }.joined(separator: "&")
        guard let url = URL(string: (server ?? "") + "/api/events?" + query + "&since=\(cursor)") else { return }
        stream = session.dataTask(with: url)
        stream?.resume()
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        guard dataTask === stream else { return }
        buffer.append(data)
        let separator = Data("\n\n".utf8)
        while let r = buffer.range(of: separator) {
            let chunk = String(decoding: buffer.subdata(in: buffer.startIndex..<r.lowerBound), as: UTF8.self)
            buffer.removeSubrange(buffer.startIndex..<r.upperBound)
            var event = ""
            var payload = ""
            for line in chunk.split(separator: "\n") {
                if line.hasPrefix("event: ") { event = String(line.dropFirst(7)) }
                if line.hasPrefix("data: ") { payload = String(line.dropFirst(6)) }
            }
            guard event == "changed",
                  let d = (try? JSONSerialization.jsonObject(with: Data(payload.utf8))) as? [String: Any],
                  let key = d["object"] as? String, let state = d["state"],
                  let seq = (d["seq"] as? NSNumber)?.uint64Value else { continue }
            deliver(key, state, seq)
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        guard task === stream else { return }
        let gen = generation
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in
            guard let self = self, self.generation == gen else { return }
            self.connect()
        }
    }
}
