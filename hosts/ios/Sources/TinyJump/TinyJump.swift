import UIKit

public struct BootError: Error, CustomStringConvertible {
    public let description: String
}

/// A custom native UIKit view registered under a name (e.g. `"MapView"`) via
/// `TinyJump.registerComponent(_:_:)` and used from `.tj` as `<Native name="MapView" ... />`.
public struct NativeComponent {
    public typealias EventSink = (_ event: String, _ value: Any?) -> Void
    public let create: (_ props: [String: Any], _ emit: @escaping EventSink) -> UIView
    public let update: (_ view: UIView, _ props: [String: Any], _ changed: [String: Any]) -> Void
    public let container: ((_ view: UIView) -> UIView?)?
    public let destroy: ((_ view: UIView) -> Void)?

    public init(
        create: @escaping (_ props: [String: Any], _ emit: @escaping EventSink) -> UIView,
        update: @escaping (_ view: UIView, _ props: [String: Any], _ changed: [String: Any]) -> Void = { _, _, _ in },
        container: ((_ view: UIView) -> UIView?)? = nil,
        destroy: ((_ view: UIView) -> Void)? = nil
    ) {
        self.create = create
        self.update = update
        self.container = container
        self.destroy = destroy
    }
}

/// Runs a tinyjump app on iOS: boots the runtime, draws its view tree with UIKit,
/// performs effects, wakes it for timers, syncs `@server` objects and (in debug builds)
/// serves automation. Use from the main thread only.
public final class TinyJump {
    /// Receives the reply to a request: `ok` on success, otherwise `error`.
    public typealias Reply = (Any?, String?) -> Void

    private static var components: [String: NativeComponent] = [:]

    /// Registers a custom native UIKit view used from `.tj` as `<Native name="..." />`.
    /// Call on the main thread before `start(...)` so the initial render finds it.
    public static func registerComponent(_ name: String, _ component: NativeComponent?) {
        if let component = component {
            components[name] = component
        } else {
            components.removeValue(forKey: name)
        }
    }

    static func nativeComponent(_ name: String) -> NativeComponent? {
        components[name]
    }

    private let native: NativeRuntime
    let renderer: Renderer
    private var sync: Sync!
    private var pending: [Int: Reply] = [:]
    private var nextReply = 1
    private var wake: DispatchWorkItem?
    private var automation: Automation?

    private init(root: UIView, bridge: HostBridge) {
        native = NativeRuntime(bridge: bridge)
        renderer = Renderer(host: root)
        bridge.renderer = renderer
    }

    /// Boots `source` into `root`. `server` is the base URL of a `tj serve` backend for `@server` objects.
    public static func start(root: UIView, source: String, server: String? = nil) throws -> TinyJump {
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("tinyjump")
        let tj = TinyJump(root: root, bridge: HostBridge(store: FileStore(dir: dir)))
        tj.sync = Sync(tj: tj, server: server)
        tj.renderer.tj = tj
        tj.renderer.root.onResize = { [weak tj] in tj?.layout() }
        // The seed drives Math.random; ids and crypto.randomId() use the `entropy` bytes below.
        // Swift's default random source is the system's secure one.
        let (code, out) = tj.native.boot(source: source, now: now(), seed: UInt64.random(in: 1...UInt64.max))
        guard code == 0 else { throw BootError(description: describeFailure(code, out)) }
        tj.apply(out)
        let bytes = (0..<32).map { _ in UInt8.random(in: 0...255) }
        tj.send(["type": "entropy", "bytes": bytes.map { String(format: "%02x", $0) }.joined()])
        if !tj.renderer.localeId.isEmpty && tj.renderer.localeId.lowercased() != "en" {
            tj.send(["type": "locale", "locale": tj.renderer.localeId])
        }
        return tj
    }

    /// Switches the active BCP-47 locale (or `"pseudo"`) and re-renders translations and formatters.
    public func setLocale(_ locale: String) {
        renderer.setLocale(locale)
        send(["type": "locale", "locale": renderer.localeId])
        layout()
    }

    /// The signed-in user's token for the server, their id and their roles as the server reports
    /// them (`GET /api/whoami`; the roles only choose what the UI shows). Call it right after
    /// `start` and whenever the token is refreshed. A new user id or new roles drop the server
    /// calls the previous identity queued and the replicas it saw (docs/access-control.md).
    public func setToken(_ token: String?, userId: String?, roles: [String]) {
        if sync.setToken(token, user: userId, roles: roles) {
            send(["type": "identity", "user": userId.map { $0 as Any } ?? NSNull(), "roles": roles])
        }
    }

    /// Forgets the token, the replicas and every queued server call.
    public func signOut() {
        setToken(nil, userId: nil, roles: [])
    }

    /// Called on the main thread when the server answers 401: refresh the token, then setToken.
    public var onTokenExpired: (() -> Void)? {
        get { sync.onTokenExpired }
        set { sync.onTokenExpired = newValue }
    }

    /// Serves /_tj/inspect, /_tj/input and /_tj/screenshot on 127.0.0.1:port.
    public func enableAutomation(port: UInt16) {
        if automation == nil {
            automation = Automation(tj: self, port: port)
        }
    }

    public func stop() {
        wake?.cancel()
        sync.stop()
        automation?.stop()
        renderer.destroy()
    }

    /// Sends an input that expects a reply (call, get, ui, inspect).
    public func request(_ input: [String: Any], reply: @escaping Reply) {
        var input = input
        input["reply"] = nextReply
        pending[nextReply] = reply
        nextReply += 1
        send(input)
    }

    func send(_ input: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: input), let json = String(data: data, encoding: .utf8) else {
            NSLog("tinyjump: input is not JSON")
            return
        }
        let (code, out) = native.handle(now: TinyJump.now(), input: json)
        if code != 0 {
            NSLog("tinyjump: rejected input: %@", out)
            return
        }
        apply(out)
    }

    /// Asks the core to lay the screen out at the root's current size, mirrored for RTL locales.
    func layout() {
        guard let size = renderer.viewportSize() else { return }
        send(["type": "layout", "width": Double(size.width), "height": Double(size.height), "rtl": renderer.rtl])
    }

    static func now() -> Double {
        Date().timeIntervalSince1970 * 1000
    }

    private static func describeFailure(_ code: Int32, _ out: String) -> String {
        let parsed = try? JSONSerialization.jsonObject(with: Data(out.utf8))
        if code == 1, let errors = parsed as? [[String: Any]] {
            return errors.compactMap { $0["text"] as? String }.joined(separator: "\n\n")
        }
        if let o = parsed as? [String: Any] {
            let trace = (o["trace"] as? [String]) ?? []
            return ([o["message"] as? String ?? out] + trace).joined(separator: "\n")
        }
        return out
    }

    private func apply(_ outputs: String) {
        guard let list = (try? JSONSerialization.jsonObject(with: Data(outputs.utf8))) as? [[String: Any]] else {
            NSLog("tinyjump: bad output from runtime")
            return
        }
        var relayout = false
        for o in list {
            switch o["type"] as? String ?? "" {
            case "patch":
                relayout = renderer.apply(o["ops"] as? [[String: Any]] ?? []) || relayout
            case "frames":
                renderer.frames(o["frames"] as? [[String: Any]] ?? [])
            case "effect":
                runEffect(id: o["id"] as? Int ?? 0, effect: o["effect"] as? [String: Any] ?? [:])
            case "subscribe":
                sync.subscribe(o["object"] as? String ?? "", seq: (o["seq"] as? NSNumber)?.uint64Value ?? 0)
            case "wake":
                schedule(at: o["at"] as? Double)
            case "reply":
                if let id = o["reply"] as? Int, let r = pending.removeValue(forKey: id) {
                    r(o["ok"], o["error"] as? String)
                }
            case "log":
                NSLog("[app] %@", o["message"] as? String ?? "")
            case "error":
                let trace = (o["trace"] as? [String] ?? []).joined(separator: "\n  ")
                NSLog("[app] error: %@\n  %@", o["message"] as? String ?? "", trace)
            default:
                break // emit / changed: no listeners on this host yet
            }
        }
        if relayout { layout() }
    }

    private func schedule(at: Double?) {
        wake?.cancel()
        guard let at = at else { return }
        let item = DispatchWorkItem { [weak self] in self?.send(["type": "tick"]) }
        wake = item
        DispatchQueue.main.asyncAfter(deadline: .now() + max(0, (at - TinyJump.now()) / 1000), execute: item)
    }

    private func effectDone(_ id: Int, _ ok: Any?, _ error: String?, field: String? = nil, unreachable: Bool = false) {
        var done: [String: Any] = ["type": "effectDone", "id": id]
        if let error = error {
            done["error"] = error
            if let field = field { done["field"] = field }
            if unreachable { done["unreachable"] = true }
        } else {
            done["ok"] = ok ?? NSNull()
        }
        send(done)
    }

    private func runEffect(id: Int, effect: [String: Any]) {
        switch effect["kind"] as? String {
        case "call":
            sync.call(effect) { [weak self] ok, error, field, unreachable in self?.effectDone(id, ok, error, field: field, unreachable: unreachable) }
        case "http":
            http(effect) { [weak self] ok, error in self?.effectDone(id, ok, error) }
        default:
            effectDone(id, nil, "unsupported effect \(effect["kind"] ?? "")")
        }
    }

    private func http(_ effect: [String: Any], done: @escaping Reply) {
        guard let s = effect["url"] as? String, s.hasPrefix("https://") || s.hasPrefix("http://"), let url = URL(string: s) else {
            done(nil, "only http(s) URLs can be fetched")
            return
        }
        var req = URLRequest(url: url, timeoutInterval: 30)
        req.httpMethod = effect["method"] as? String ?? "GET"
        for (k, v) in effect["headers"] as? [String: String] ?? [:] {
            req.setValue(v, forHTTPHeaderField: k)
        }
        if let body = effect["body"] as? String {
            req.httpBody = Data(body.utf8)
        }
        URLSession.shared.dataTask(with: req) { data, response, error in
            DispatchQueue.main.async {
                guard let http = response as? HTTPURLResponse, error == nil else {
                    done(nil, error?.localizedDescription ?? "request failed")
                    return
                }
                var headers: [String: String] = [:]
                for (k, v) in http.allHeaderFields {
                    headers["\(k)".lowercased()] = "\(v)"
                }
                let body = String(decoding: data ?? Data(), as: UTF8.self)
                done(["status": http.statusCode, "headers": headers, "body": body], nil)
            }
        }.resume()
    }
}
