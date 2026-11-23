import AppKit
import Foundation

/// Runs any `.tj` / `.tjb` app or bundled example natively on macOS with AppKit.
///
/// Launch arguments:
///   --app <path/to/app.tj>   Load an app directly from disk (bundles sibling .tj files)
///   --example <name>         Load a bundled example (default: todo)
///   --server <url>           Base URL of a `tj serve` backend for `@server` objects
///   --dir rtl|ltr            Override layout direction
///   --key <api-key>          Sign in via `GET /api/whoami`
///   --data <dir>             Isolate local object storage directory
///   --port <port>            Debug automation port on 127.0.0.1 (default: 4702)
///   --width <pt> --height <pt>  Initial window size (default: 960x680)
@main
final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate {
    private var window: NSWindow?
    private var app: TinyJump?

    static func main() {
        let nsApp = NSApplication.shared
        nsApp.setActivationPolicy(.regular)
        let delegate = AppDelegate()
        nsApp.delegate = delegate
        withExtendedLifetime(delegate) {
            nsApp.run()
        }
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        setupMenu()
        let args = parseArgs(CommandLine.arguments)
        let width = args["width"].flatMap(Double.init) ?? 960
        let height = args["height"].flatMap(Double.init) ?? 680

        let win = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: width, height: height),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        win.title = args["title"] ?? (args["example"].map { "TinyJump — \($0)" } ?? "TinyJump")
        win.delegate = self
        win.center()

        let content = FlippedView(frame: NSRect(x: 0, y: 0, width: width, height: height))
        content.wantsLayer = true
        content.layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
        switch args["dir"] {
        case "rtl": content.userInterfaceLayoutDirection = .rightToLeft
        case "ltr": content.userInterfaceLayoutDirection = .leftToRight
        default: break
        }
        win.contentView = content
        win.makeKeyAndOrderFront(nil)
        self.window = win

        let source: String
        if let path = args["app"] {
            guard let loaded = loadFromDisk(URL(fileURLWithPath: path)) else {
                fputs("tinyjump-macos: cannot load app at \(path)\n", stderr)
                NSApp.terminate(nil)
                return
            }
            source = loaded
        } else {
            let example = args["example"] ?? "todo"
            guard let loaded = loadBundledExample(example) else {
                fputs("tinyjump-macos: missing example \(example)\n", stderr)
                NSApp.terminate(nil)
                return
            }
            source = loaded
        }

        let dataDir = args["data"].map { URL(fileURLWithPath: $0, isDirectory: true) }
        do {
            app = try TinyJump.start(root: content, source: source, server: args["server"], dataDir: dataDir)
        } catch {
            fputs("tinyjump-macos: boot failed: \(error)\n", stderr)
            NSApp.terminate(nil)
            return
        }

        #if DEBUG
        let port = args["port"].flatMap(UInt16.init) ?? 4702
        app?.enableAutomation(port: port)
        #endif

        if let server = args["server"], let key = args["key"] {
            signIn(server: server, key: key)
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    func applicationWillTerminate(_ notification: Notification) {
        app?.stop()
    }

    private func setupMenu() {
        let mainMenu = NSMenu()
        let appItem = NSMenuItem()
        mainMenu.addItem(appItem)
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "Quit TinyJump", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appItem.submenu = appMenu
        NSApp.mainMenu = mainMenu
    }

    private func parseArgs(_ raw: [String]) -> [String: String] {
        var out: [String: String] = [:]
        var i = 1
        while i < raw.count {
            let cur = raw[i]
            let key: String?
            if cur.hasPrefix("--") {
                key = String(cur.dropFirst(2))
            } else if cur.hasPrefix("-") {
                key = String(cur.dropFirst(1))
            } else {
                key = nil
            }
            if let k = key, i + 1 < raw.count {
                out[k] = raw[i + 1]
                i += 2
            } else {
                i += 1
            }
        }
        return out
    }

    private func signIn(server: String, key: String) {
        guard let url = URL(string: server.trimmingCharacters(in: CharacterSet(charactersIn: "/")) + "/api/whoami") else { return }
        var req = URLRequest(url: url)
        req.setValue("Bearer " + key, forHTTPHeaderField: "Authorization")
        URLSession.shared.dataTask(with: req) { data, _, _ in
            guard let data = data,
                  let body = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
                  let me = body["ok"] as? [String: Any],
                  let id = me["id"] as? String, let roles = me["roles"] as? [String] else {
                NSLog("tinyjump-macos: sign-in failed")
                return
            }
            DispatchQueue.main.async { self.app?.setToken(key, userId: id, roles: roles) }
        }.resume()
    }

    private func loadFromDisk(_ entry: URL) -> String? {
        if entry.pathExtension == "tjb" {
            return try? String(contentsOf: entry, encoding: .utf8)
        }
        let dir = entry.deletingLastPathComponent()
        let entryName = entry.lastPathComponent
        guard let paths = FileManager.default.subpaths(atPath: dir.path) else {
            return try? String(contentsOf: entry, encoding: .utf8)
        }
        let others = paths.filter { $0.hasSuffix(".tj") && $0 != entryName && !$0.hasPrefix(".") }.sorted()
        let locales = paths.filter { $0.hasPrefix("locales/") && $0.hasSuffix(".json") }.sorted()
        let names = [entryName] + others + locales
        let texts = names.compactMap { try? String(contentsOf: dir.appendingPathComponent($0), encoding: .utf8) }
        guard texts.count == names.count else { return nil }
        if texts.count == 1 { return texts[0] }
        return zip(names, texts).map { name, text in "//# file: \(name)\n\(text)\(text.hasSuffix("\n") ? "" : "\n")" }.joined()
    }

    private func loadBundledExample(_ example: String) -> String? {
        guard let dir = Bundle.main.resourceURL?.appendingPathComponent("examples/\(example)"),
              let paths = FileManager.default.subpaths(atPath: dir.path) else { return nil }
        let names = ["app.tj"] + paths.filter { $0.hasSuffix(".tj") && $0 != "app.tj" }.sorted()
        let texts = names.compactMap { try? String(contentsOf: dir.appendingPathComponent($0), encoding: .utf8) }
        guard texts.count == names.count else { return nil }
        if texts.count == 1 { return texts[0] }
        return zip(names, texts).map { name, text in "//# file: \(name)\n\(text)\(text.hasSuffix("\n") ? "" : "\n")" }.joined()
    }
}
