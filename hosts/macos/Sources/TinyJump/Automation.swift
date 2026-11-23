import Foundation
import Network

/// Debug automation on 127.0.0.1, the same endpoints as `tj serve`, Android, and iOS:
/// GET /_tj/inspect, POST /_tj/input, plus GET /_tj/screenshot (PNG) for visual tests.
final class Automation {
    private static let maxRequest = 1 << 20
    private weak var tj: TinyJump?
    private var listener: NWListener?

    init(tj: TinyJump, port: UInt16) {
        self.tj = tj
        let params = NWParameters.tcp
        params.requiredInterfaceType = .loopback
        guard let p = NWEndpoint.Port(rawValue: port), let l = try? NWListener(using: params, on: p) else {
            NSLog("tinyjump-macos: automation could not listen on %d", Int(port))
            return
        }
        l.newConnectionHandler = { [weak self] c in self?.serve(c) }
        l.start(queue: .global())
        listener = l
        NSLog("tinyjump-macos: automation on 127.0.0.1:%d", Int(port))
    }

    func stop() {
        listener?.cancel()
    }

    private func serve(_ c: NWConnection) {
        c.start(queue: .global())
        read(c, Data())
    }

    private func read(_ c: NWConnection, _ received: Data) {
        c.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, done, error in
            guard let self = self else { return }
            var buf = received
            if let data = data { buf.append(data) }
            if let req = Request(buf) {
                DispatchQueue.main.async { self.handle(req, c) }
            } else if done || error != nil || buf.count > Automation.maxRequest {
                c.cancel()
            } else {
                self.read(c, buf)
            }
        }
    }

    private struct Request {
        let method: String
        let path: String
        let body: Data

        init?(_ buf: Data) {
            guard let end = buf.range(of: Data("\r\n\r\n".utf8)) else { return nil }
            let head = String(decoding: buf.subdata(in: buf.startIndex..<end.lowerBound), as: UTF8.self)
            let lines = head.components(separatedBy: "\r\n")
            let first = lines.first?.split(separator: " ") ?? []
            var length = 0
            for line in lines.dropFirst() {
                let parts = line.split(separator: ":", maxSplits: 1)
                if parts.count == 2, parts[0].trimmingCharacters(in: .whitespaces).lowercased() == "content-length" {
                    length = Int(parts[1].trimmingCharacters(in: .whitespaces)) ?? 0
                }
            }
            let bodyStart = end.upperBound
            guard length >= 0, buf.endIndex - bodyStart >= length, first.count >= 2 else { return nil }
            method = String(first[0])
            path = String(first[1])
            body = buf.subdata(in: bodyStart..<(bodyStart + length))
        }
    }

    private func handle(_ req: Request, _ c: NWConnection) {
        guard let tj = tj else { return c.cancel() }
        let reply: (Int, [String: Any]) -> Void = { status, json in
            let body = (try? JSONSerialization.data(withJSONObject: json)) ?? Data()
            self.respond(c, status, "application/json", body)
        }
        let finish: TinyJump.Reply = { ok, error in
            if let error = error {
                reply(400, ["error": error])
            } else {
                reply(200, ["ok": ok ?? NSNull()])
            }
        }
        switch (req.method, req.path) {
        case ("GET", "/_tj/inspect"):
            tj.request(["type": "inspect"]) { ok, error in
                guard var snapshot = ok as? [String: Any] else { return finish(ok, error) }
                snapshot["tree"] = tj.renderer.addBounds(snapshot["tree"] as? [[String: Any]] ?? [])
                snapshot["viewport"] = tj.renderer.viewport()
                finish(snapshot, nil)
            }
        case ("GET", "/_tj/screenshot"):
            if let png = tj.renderer.screenshot() {
                respond(c, 200, "image/png", png)
            } else {
                reply(500, ["error": "no window to capture"])
            }
        case ("POST", "/_tj/input"):
            guard let raw = (try? JSONSerialization.jsonObject(with: req.body)) as? [String: Any] else {
                return reply(400, ["error": "body must be one protocol input"])
            }
            let input = tj.renderer.forAgent(raw)
            if ["call", "get", "ui", "inspect"].contains(input["type"] as? String ?? "") {
                tj.request(input, reply: finish)
            } else if input["type"] as? String == "locale" {
                tj.setLocale(input["locale"] as? String ?? "")
                finish(NSNull(), nil)
            } else {
                tj.send(input)
                finish(NSNull(), nil)
            }
        default:
            reply(404, ["error": "not found"])
        }
    }

    private func respond(_ c: NWConnection, _ status: Int, _ type: String, _ body: Data) {
        var data = Data("HTTP/1.1 \(status) \(status == 200 ? "OK" : "Error")\r\nContent-Type: \(type)\r\nContent-Length: \(body.count)\r\nConnection: close\r\n\r\n".utf8)
        data.append(body)
        c.send(content: data, completion: .contentProcessed { _ in c.cancel() })
    }
}
