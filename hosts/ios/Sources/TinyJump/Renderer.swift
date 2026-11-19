import UIKit

/// Turns UIKit control events into runtime UI events for one node.
final class EventTarget: NSObject, UITextFieldDelegate {
    let fire: (String, Any) -> Void

    init(fire: @escaping (String, Any) -> Void) {
        self.fire = fire
    }

    @objc func press() { fire("onPress", NSNull()) }
    @objc func changed(_ sender: UITextField) { fire("onChange", sender.text ?? "") }
    @objc func toggled(_ sender: UISwitch) { fire("onChange", sender.isOn) }

    func textFieldShouldReturn(_ textField: UITextField) -> Bool {
        fire("onSubmit", textField.text ?? "")
        return true
    }
}

/// The app's root: reports size changes so the core can lay the screen out again.
final class RootView: UIView {
    var onResize: (() -> Void)?
    private var last = CGSize.zero

    override func layoutSubviews() {
        super.layoutSubviews()
        if bounds.size != last {
            last = bounds.size
            onResize?()
        }
    }
}

/// A label that draws its own padding (leaf padding is part of a leaf's measured size).
final class PaddedLabel: UILabel {
    var insets = UIEdgeInsets.zero

    override func drawText(in rect: CGRect) {
        super.drawText(in: rect.inset(by: insets))
    }

    override func sizeThatFits(_ size: CGSize) -> CGSize {
        let inner = CGSize(width: max(0, size.width - insets.left - insets.right), height: max(0, size.height - insets.top - insets.bottom))
        let s = super.sizeThatFits(inner)
        return CGSize(width: s.width + insets.left + insets.right, height: s.height + insets.top + insets.bottom)
    }
}

/// A switch with a label beside it, measured and laid out as one widget.
final class ToggleView: UIView {
    let toggle = UISwitch()
    let label = UILabel()
    private let spacing: CGFloat = 8

    override init(frame: CGRect) {
        super.init(frame: frame)
        label.numberOfLines = 0
        addSubview(toggle)
        addSubview(label)
    }

    required init?(coder: NSCoder) {
        fatalError("not used from storyboards")
    }

    override func sizeThatFits(_ size: CGSize) -> CGSize {
        let sw = toggle.intrinsicContentSize
        let l = label.sizeThatFits(CGSize(width: max(0, size.width - sw.width - spacing), height: .greatestFiniteMagnitude))
        return CGSize(width: sw.width + spacing + l.width, height: max(sw.height, l.height))
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        let sw = toggle.intrinsicContentSize
        let labelWidth = max(0, bounds.width - sw.width - spacing)
        // The switch sits at the start: the left, or the right in right-to-left layouts.
        let rtl = effectiveUserInterfaceLayoutDirection == .rightToLeft
        toggle.frame = CGRect(x: rtl ? bounds.width - sw.width : 0, y: (bounds.height - sw.height) / 2, width: sw.width, height: sw.height)
        label.frame = CGRect(x: rtl ? 0 : sw.width + spacing, y: 0, width: labelWidth, height: bounds.height)
    }
}

/// Applies view-tree patches to UIKit views and places them where the core's layout says
/// (crates/tj-core/src/ui/layout.rs). Mirrors the catalog in crates/tj-core/src/ui/catalog.rs.
final class Renderer {
    final class Node {
        let id: Int
        let kind: String
        let view: UIView
        /// Where children go; nil for leaf widgets.
        let container: UIView?
        var target: EventTarget?
        var props: [String: Any] = [:]
        var events: [String] = []
        /// True while the renderer itself changes a control, so its events stay quiet.
        var quiet = false

        init(id: Int, kind: String, view: UIView, container: UIView?) {
            self.id = id
            self.kind = kind
            self.view = view
            self.container = container
        }
    }

    weak var tj: TinyJump?
    let root = RootView()
    private var nodes: [Int: Node] = [:]

    /// The root follows `host`'s direction: set `host.semanticContentAttribute` to
    /// `.forceRightToLeft` (or `.forceLeftToRight`) to override the app's language.
    init(host: UIView) {
        root.semanticContentAttribute = host.semanticContentAttribute
        root.translatesAutoresizingMaskIntoConstraints = false
        host.addSubview(root)
        let safe = host.safeAreaLayoutGuide
        NSLayoutConstraint.activate([
            root.topAnchor.constraint(equalTo: safe.topAnchor),
            root.leadingAnchor.constraint(equalTo: safe.leadingAnchor),
            root.trailingAnchor.constraint(equalTo: safe.trailingAnchor),
            root.bottomAnchor.constraint(equalTo: safe.bottomAnchor),
        ])
        nodes[0] = Node(id: 0, kind: "Root", view: root, container: root)
    }

    /// Applies a patch; true if anything changed, so the screen needs a new layout.
    func apply(_ ops: [[String: Any]]) -> Bool {
        for op in ops {
            guard let id = op["id"] as? Int else { continue }
            switch op["op"] as? String ?? "" {
            case "create":
                let props = op["props"] as? [String: Any] ?? [:]
                let n = create(id: id, kind: op["kind"] as? String ?? "", props: props)
                inheritDirection(n.view)
                n.events = op["events"] as? [String] ?? []
                nodes[id] = n
                setProps(n, props)
            case "update":
                guard let n = nodes[id] else { continue }
                setProps(n, op["props"] as? [String: Any] ?? [:])
                if let events = op["events"] as? [String] { n.events = events }
            case "children":
                guard let box = nodes[id]?.container else { continue }
                let views = (op["ids"] as? [Int] ?? []).compactMap { nodes[$0]?.view }
                for v in box.subviews where !views.contains(v) { v.removeFromSuperview() }
                for (i, v) in views.enumerated() where box.subviews.firstIndex(of: v) != i { box.insertSubview(v, at: i) }
            case "remove":
                if let n = nodes.removeValue(forKey: id) { n.view.removeFromSuperview() }
            default:
                break
            }
        }
        return !ops.isEmpty
    }

    /// Views take the root's direction, so text alignment and widget parts mirror with it.
    private func inheritDirection(_ view: UIView) {
        view.semanticContentAttribute = root.semanticContentAttribute
        view.subviews.forEach(inheritDirection)
    }

    /// Whether the screen reads right to left (the app's language, or the host's override).
    var rtl: Bool { root.effectiveUserInterfaceLayoutDirection == .rightToLeft }

    /// Places nodes at the frames the core computed (points, relative to their parent).
    func frames(_ list: [[String: Any]]) {
        for f in list {
            guard let id = f["id"] as? Int, let n = nodes[id] else { continue }
            let num = { (k: String) in CGFloat((f[k] as? Double) ?? 0) }
            n.view.frame = CGRect(x: num("x"), y: num("y"), width: num("w"), height: num("h"))
            if let content = f["content"] as? Double, let scroll = n.view as? UIScrollView, let box = n.container {
                box.frame = CGRect(x: 0, y: 0, width: num("w"), height: CGFloat(content))
                scroll.contentSize = box.frame.size
            }
        }
    }

    /// The natural size of a leaf in points, wrapping at `maxWidth`.
    func measure(node: Int, maxWidth: CGFloat?) -> CGSize {
        guard let view = nodes[node]?.view else { return .zero }
        let s = view.sizeThatFits(CGSize(width: maxWidth ?? .greatestFiniteMagnitude, height: .greatestFiniteMagnitude))
        let width = maxWidth.map { min(s.width, $0) } ?? s.width
        // Round up, with a point of slack: layout snaps to whole points, and text a fraction
        // too narrow would wrap its last word onto a line nobody sees.
        return CGSize(width: ceil(width) + 1, height: ceil(s.height))
    }

    /// The root's size in points, or nil before it has one.
    func viewportSize() -> CGSize? {
        root.bounds.width > 0 && root.bounds.height > 0 ? root.bounds.size : nil
    }

    // ---------- widgets ----------

    private func create(id: Int, kind: String, props: [String: Any]) -> Node {
        switch kind {
        case "Column", "Row":
            let box = UIView()
            return Node(id: id, kind: kind, view: box, container: box)
        case "Scroll":
            let scroll = UIScrollView()
            let content = UIView()
            scroll.addSubview(content)
            return Node(id: id, kind: kind, view: scroll, container: content)
        case "Button":
            let b = UIButton(type: .system)
            let n = Node(id: id, kind: kind, view: b, container: nil)
            b.addTarget(target(for: n), action: #selector(EventTarget.press), for: .touchUpInside)
            return n
        case "Input":
            let f = UITextField()
            f.borderStyle = .roundedRect
            f.textAlignment = .natural
            let n = Node(id: id, kind: kind, view: f, container: nil)
            let t = target(for: n)
            f.addTarget(t, action: #selector(EventTarget.changed(_:)), for: .editingChanged)
            f.delegate = t
            return n
        case "Toggle":
            let v = ToggleView()
            let n = Node(id: id, kind: kind, view: v, container: nil)
            v.toggle.addTarget(target(for: n), action: #selector(EventTarget.toggled(_:)), for: .valueChanged)
            return n
        case "Image":
            let img = UIImageView()
            img.contentMode = .scaleAspectFit
            img.clipsToBounds = true
            return Node(id: id, kind: kind, view: img, container: nil)
        case "Text":
            let label = PaddedLabel()
            label.numberOfLines = 0
            label.textAlignment = .natural
            return Node(id: id, kind: kind, view: label, container: nil)
        default: // Spacer
            return Node(id: id, kind: kind, view: UIView(), container: nil)
        }
    }

    private func target(for n: Node) -> EventTarget {
        let t = EventTarget { [weak self, weak n] event, value in
            guard let self = self, let n = n, !n.quiet, n.events.contains(event) else { return }
            self.tj?.send(["type": "ui", "target": ["node": n.id], "event": event, "value": value])
        }
        n.target = t
        return t
    }

    private func setProps(_ n: Node, _ props: [String: Any]) {
        n.quiet = true
        defer { n.quiet = false }
        for (k, v) in props {
            let none = v is NSNull
            if none { n.props.removeValue(forKey: k) } else { n.props[k] = v }
            setProp(n, k, none ? nil : v)
        }
        if props.keys.contains(where: { ["background", "radius", "border"].contains($0) }) { restyle(n) }
        if props.keys.contains(where: { ["size", "weight"].contains($0) }) { refont(n) }
    }

    /// Visual props. Layout props (width, gap, align, grow, ...) are the core's job.
    private func setProp(_ n: Node, _ k: String, _ v: Any?) {
        let view = n.view
        let label = (view as? UILabel) ?? (view as? ToggleView)?.label
        switch k {
        case "text":
            let s = v.map { "\($0)" } ?? ""
            if let b = view as? UIButton { b.setTitle(s, for: .normal) }
            label?.text = s
        case "value":
            if let t = view as? ToggleView {
                t.toggle.setOn((v as? Bool) ?? false, animated: false)
            } else if let f = view as? UITextField {
                let s = v.map { "\($0)" } ?? ""
                if f.text != s { f.text = s }
            }
        case "placeholder":
            (view as? UITextField)?.placeholder = v.map { "\($0)" }
        case "disabled":
            let enabled = !((v as? Bool) ?? false)
            (view as? UIControl)?.isEnabled = enabled
            (view as? ToggleView)?.toggle.isEnabled = enabled
        case "secure":
            (view as? UITextField)?.isSecureTextEntry = (v as? Bool) ?? false
        case "lines":
            label?.numberOfLines = (v as? Int) ?? 0
        case "src":
            if let img = view as? UIImageView { load(img, v as? String) }
        case "fit":
            let fit = v as? String
            view.contentMode = fit == "cover" ? .scaleAspectFill : fit == "fill" ? .scaleToFill : .scaleAspectFit
        case "id":
            view.accessibilityIdentifier = v as? String
        case "label":
            view.accessibilityLabel = v.map { "\($0)" }
        case "color":
            let c = v.flatMap { Renderer.color("\($0)") }
            label?.textColor = c ?? .label
            (view as? UITextField)?.textColor = c ?? .label
            if let b = view as? UIButton { b.setTitleColor(c, for: .normal) }
        case "padding":
            // Containers are padded by the core's layout; leaves draw their own padding.
            let p = CGFloat((v as? Double) ?? 0)
            let insets = UIEdgeInsets(top: p, left: p, bottom: p, right: p)
            (view as? PaddedLabel)?.insets = insets
            (view as? UIButton)?.contentEdgeInsets = insets
        default:
            break
        }
    }

    private func refont(_ n: Node) {
        let size = CGFloat((n.props["size"] as? Double) ?? Double(UIFont.labelFontSize))
        let w = n.props["weight"]
        let bold = (w as? String).map { $0 == "bold" || $0 == "bolder" } ?? (((w as? Double) ?? 400) >= 600)
        let font = UIFont.systemFont(ofSize: size, weight: bold ? .bold : .regular)
        ((n.view as? UILabel) ?? (n.view as? ToggleView)?.label)?.font = font
        (n.view as? UITextField)?.font = font
        (n.view as? UIButton)?.titleLabel?.font = font
    }

    private func restyle(_ n: Node) {
        n.view.backgroundColor = (n.props["background"] as? String).flatMap(Renderer.color)
        let radius = CGFloat((n.props["radius"] as? Double) ?? 0)
        n.view.layer.cornerRadius = radius
        n.view.clipsToBounds = radius > 0 || n.view is UIImageView
        switch n.props["border"] ?? NSNull() {
        case let width as Double:
            n.view.layer.borderWidth = CGFloat(width)
            n.view.layer.borderColor = UIColor.label.cgColor
        case let spec as String:
            var width: CGFloat = 1
            var color = UIColor.label
            for part in spec.split(separator: " ") {
                if part.hasSuffix("px"), let w = Double(part.dropLast(2)) { width = CGFloat(w) }
                if let c = Renderer.color(String(part)) { color = c }
            }
            n.view.layer.borderWidth = width
            n.view.layer.borderColor = color.cgColor
        default:
            n.view.layer.borderWidth = 0
        }
    }

    private func load(_ img: UIImageView, _ src: String?) {
        img.image = nil
        guard let src = src, src.hasPrefix("https://") || src.hasPrefix("http://"), let url = URL(string: src) else { return }
        URLSession.shared.dataTask(with: url) { data, _, _ in
            guard let data = data, let image = UIImage(data: data) else { return }
            DispatchQueue.main.async { img.image = image }
        }.resume()
    }

    /// CSS-style colors: #rgb, #rrggbb, #rrggbbaa, or a few names.
    static func color(_ s: String) -> UIColor? {
        let names: [String: UIColor] = ["black": .black, "white": .white, "red": .red, "green": .green, "blue": .blue, "gray": .gray, "grey": .gray, "transparent": .clear]
        if let named = names[s.lowercased()] { return named }
        guard s.hasPrefix("#") else { return nil }
        var hex = String(s.dropFirst())
        if hex.count == 3 { hex = hex.map { "\($0)\($0)" }.joined() }
        if hex.count == 6 { hex += "ff" }
        guard hex.count == 8, let v = UInt64(hex, radix: 16) else { return nil }
        return UIColor(red: CGFloat((v >> 24) & 0xff) / 255, green: CGFloat((v >> 16) & 0xff) / 255, blue: CGFloat((v >> 8) & 0xff) / 255, alpha: CGFloat(v & 0xff) / 255)
    }

    // ---------- automation ----------

    /// The window size in points, for overflow checks.
    func viewport() -> [String: Int] {
        let size = root.window?.bounds.size ?? .zero
        return ["w": Int(size.width), "h": Int(size.height)]
    }

    /// Adds bounds (points, on screen) and visibility to an inspect tree, like the other hosts.
    func addBounds(_ tree: [[String: Any]]) -> [[String: Any]] {
        tree.map { node in
            var n = node
            if let id = n["id"] as? Int, let v = nodes[id]?.view {
                let frame = v.convert(v.bounds, to: nil)
                let screen = v.window?.bounds ?? .zero
                let visible = v.window != nil && !v.isHidden && frame.width > 0 && frame.height > 0 && frame.intersects(screen)
                n["bounds"] = ["x": Int(frame.minX.rounded()), "y": Int(frame.minY.rounded()), "w": Int(frame.width.rounded()), "h": Int(frame.height.rounded())]
                n["visible"] = visible
            }
            n["children"] = addBounds(n["children"] as? [[String: Any]] ?? [])
            return n
        }
    }

    /// The whole screen as PNG, for visual tests.
    func screenshot() -> Data? {
        guard let window = root.window else { return nil }
        let renderer = UIGraphicsImageRenderer(bounds: window.bounds)
        let image = renderer.image { _ in window.drawHierarchy(in: window.bounds, afterScreenUpdates: true) }
        return image.pngData()
    }
}
