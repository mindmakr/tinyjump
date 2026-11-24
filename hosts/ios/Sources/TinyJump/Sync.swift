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
    /// Sent as `Authorization: Bearer` (docs/access-control.md); nil when signed out.
    private var token: String?
    private var user: String?
    private var roles: [String] = []
    /// Called when the server answers 401, so the embedding app can refresh the token.
    var onTokenExpired: (() -> Void)?

    init(tj: TinyJump, server: String?) {
        self.tj = tj
        self.server = server.map { $0.hasSuffix("/") ? String($0.dropLast()) : $0 }
        super.init()
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 60 // the server sends a keep-alive every 15s
        session = URLSession(configuration: config, delegate: self, delegateQueue: OperationQueue.main)
    }

    private static func covers(_ pattern: String, _ key: String) -> Bool {
        pattern.hasSuffix("*") && key.hasPrefix(String(pattern.dropLast()))
    }

    /// Starts keeping `key` current; `seq` is the change number of its cached state.
    func subscribe(_ key: String, seq: UInt64) {
        guard server != nil, replicas[key] == nil else { return }
        // Already covered by a broader class or topic stream (`Slot/*`, `Room/studio:*`).
        if replicas.keys.contains(where: { Sync.covers($0, key) }) { return }
        if key.hasSuffix("*") {
            // A broader topic or class stream covers narrower streams and individual objects.
            replicas = replicas.filter { !Sync.covers(key, $0.key) }
        }
        replicas[key] = seq
        cursor = replicas.values.min() ?? 0
        connect()
    }

    func stop() {
        generation += 1
        stream?.cancel()
        session.invalidateAndCancel()
    }

    /// Calls a method of an `@server` object; `done` gets its result or an error, and whether
    /// the server was unreachable (no answer, a 5xx or an unreadable reply), in which case the
    /// runtime retries. The call id makes retries safe: the server runs each call once.
    func call(_ effect: [String: Any], done: @escaping (Any?, String?, String?, Bool) -> Void) {
        let object = effect["object"] as? String ?? ""
        guard server != nil, let url = URL(string: objectUrl(object) + "/" + Sync.encode(effect["method"] as? String ?? "")) else {
            done(nil, "no server configured for \(object)", nil, false)
            return
        }
        var req = URLRequest(url: url, timeoutInterval: 30)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue(effect["call"] as? String, forHTTPHeaderField: "Idempotency-Key")
        authorize(&req)
        req.httpBody = try? JSONSerialization.data(withJSONObject: effect["args"] as? [Any] ?? [])
        URLSession.shared.dataTask(with: req) { data, response, error in
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            // A 502+, 401 or 429 is retried (gateway/overload, token refresh, or rate-limit backoff);
            // a JSON 500 with `{"error": ...}` fails deterministically instead of wedging `tj:outbox`.
            let reply = status < 502 && status != 401 && status != 429 ? data.flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String: Any] : nil
            DispatchQueue.main.async {
                if status == 401 { self.onTokenExpired?() }
                if let message = reply?["error"] as? String {
                    done(nil, message, reply?["field"] as? String, false)
                } else if (200..<300).contains(status), let reply = reply {
                    done(reply["ok"], nil, nil, false)
                } else {
                    done(nil, error?.localizedDescription ?? "the server did not answer", nil, true)
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
        var matched = false
        for (entry, have) in replicas where entry == key || Sync.covers(entry, key) {
            replicas[entry] = max(have, seq)
            matched = true
        }
        guard matched else { return }
        cursor = max(cursor, seq) // the stream is in change order
        tj?.send(["type": "sync", "object": key, "state": state, "seq": seq])
    }

    /// Sets the token; returns true if the user or their roles changed, in which case replicas
    /// start over (the runtime drops what the previous user queued, and resubscribes).
    func setToken(_ newToken: String?, user newUser: String?, roles newRoles: [String]) -> Bool {
        token = newToken
        let changed = newUser != user || newRoles != roles
        user = newUser
        roles = newRoles
        if changed {
            replicas = [:]
            cursor = 0
            generation += 1
            stream?.cancel()
            stream = nil
        } else if !replicas.isEmpty {
            connect()
        }
        return changed
    }

    private func authorize(_ req: inout URLRequest) {
        if let token = token { req.setValue("Bearer " + token, forHTTPHeaderField: "Authorization") }
    }

    /// One stream for all replicas, replaced when the set grows, resuming from `cursor`.
    private func connect() {
        generation += 1
        stream?.cancel()
        buffer = Data()
        let query = replicas.keys.map { "object=" + Sync.encode($0) }.joined(separator: "&")
        guard let url = URL(string: (server ?? "") + "/api/events?" + query + "&since=\(cursor)") else { return }
        var req = URLRequest(url: url)
        authorize(&req)
        stream = session.dataTask(with: req)
        stream?.resume()
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse, completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
        let status = (response as? HTTPURLResponse)?.statusCode
        if dataTask === stream, status == 200 {
            tj?.send(["type": "online"]) // reachable again: queued calls go now, not after their backoff
        }
        if dataTask === stream, status == 401 { onTokenExpired?() }
        completionHandler(.allow)
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
