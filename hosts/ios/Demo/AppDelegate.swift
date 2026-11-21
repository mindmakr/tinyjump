import UIKit

/// Runs one of the repo's example apps. Launch arguments: `-example <name>` (default todo),
/// `-dir rtl|ltr` (overriding the language's direction), `-server <url>` for `@server` objects and
/// `-key <api key>` from `tj key` (the demo signs in with it, as an app would with its own sign-in).
/// Debug builds serve automation on port 4701.
@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?
    private var app: TinyJump?

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        let controller = UIViewController()
        controller.view.backgroundColor = .systemBackground
        let window = UIWindow(frame: UIScreen.main.bounds)
        window.rootViewController = controller
        window.makeKeyAndVisible()
        self.window = window

        let defaults = UserDefaults.standard
        switch defaults.string(forKey: "dir") {
        case "rtl": controller.view.semanticContentAttribute = .forceRightToLeft
        case "ltr": controller.view.semanticContentAttribute = .forceLeftToRight
        default: break
        }
        let example = defaults.string(forKey: "example") ?? "todo"
        guard let source = AppDelegate.source(example) else {
            fatalError("missing example \(example)")
        }
        do {
            app = try TinyJump.start(root: controller.view, source: source, server: defaults.string(forKey: "server"))
        } catch {
            fatalError("\(error)")
        }
        #if DEBUG
        app?.enableAutomation(port: 4701)
        #endif
        if let server = defaults.string(forKey: "server"), let key = defaults.string(forKey: "key") {
            signIn(server: server, key: key)
        }
        return true
    }

    /// Asks the server who the key belongs to (`GET /api/whoami`), then hands it to the runtime.
    private func signIn(server: String, key: String) {
        guard let url = URL(string: server.trimmingCharacters(in: CharacterSet(charactersIn: "/")) + "/api/whoami") else { return }
        var req = URLRequest(url: url)
        req.setValue("Bearer " + key, forHTTPHeaderField: "Authorization")
        URLSession.shared.dataTask(with: req) { data, _, _ in
            guard let data = data,
                  let body = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
                  let me = body["ok"] as? [String: Any],
                  let id = me["id"] as? String, let roles = me["roles"] as? [String] else {
                NSLog("tinyjump-demo: sign-in failed")
                return
            }
            DispatchQueue.main.async { self.app?.setToken(key, userId: id, roles: roles) }
        }.resume()
    }

    /// The example's files as one source: app.tj first, then its other .tj files, each after a
    /// `//# file: <path>` line (the bundle format, see tj-core's modules.rs).
    private static func source(_ example: String) -> String? {
        guard let dir = Bundle.main.resourceURL?.appendingPathComponent("examples/\(example)"),
              let paths = FileManager.default.subpaths(atPath: dir.path) else { return nil }
        let names = ["app.tj"] + paths.filter { $0.hasSuffix(".tj") && $0 != "app.tj" }.sorted()
        let texts = names.compactMap { try? String(contentsOf: dir.appendingPathComponent($0), encoding: .utf8) }
        guard texts.count == names.count else { return nil }
        if texts.count == 1 { return texts[0] }
        return zip(names, texts).map { name, text in "//# file: \(name)\n\(text)\(text.hasSuffix("\n") ? "" : "\n")" }.joined()
    }
}
