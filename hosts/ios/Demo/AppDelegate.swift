import UIKit

/// Runs one of the repo's example apps. Launch arguments: `-example <name>` (default todo)
/// and `-server <url>` for `@server` objects. Debug builds serve automation on port 4701.
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
        let example = defaults.string(forKey: "example") ?? "todo"
        guard let url = Bundle.main.url(forResource: "app", withExtension: "tj", subdirectory: "examples/\(example)"),
              let source = try? String(contentsOf: url, encoding: .utf8) else {
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
        return true
    }
}
