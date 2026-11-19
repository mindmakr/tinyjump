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

/// Applies view-tree patches to UIKit views. Mirrors crates/tj-core/src/ui/catalog.rs.
final class Renderer {
    final class Node {
        let id: Int
        let kind: String
        let view: UIView
        /// Where children go; nil for leaf widgets.
        let container: UIStackView?
        /// The label and switch of a Toggle.
        var label: UILabel?
        var toggle: UISwitch?
        var target: EventTarget?
        var props: [String: Any] = [:]
        var events: [String] = []
        var children: [Int] = []
        var parent = -1
        /// True while the renderer itself changes a control, so its events stay quiet.
        var quiet = false
        var size: [NSLayoutConstraint] = []
        /// Takes up the free space at the end of a stack, so children keep their size.
        let filler = UIView()

        init(id: Int, kind: String, view: UIView, container: UIStackView?) {
            self.id = id
            self.kind = kind
            self.view = view
            self.container = container
        }
    }

    weak var tj: TinyJump?
    private var nodes: [Int: Node] = [:]

    init(root: UIView) {
        let stack = UIStackView()
        stack.axis = .vertical
        stack.alignment = .fill
        stack.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(stack)
        let safe = root.safeAreaLayoutGuide
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: safe.topAnchor),
            stack.leadingAnchor.constraint(equalTo: safe.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: safe.trailingAnchor),
            stack.bottomAnchor.constraint(lessThanOrEqualTo: safe.bottomAnchor),
        ])
        nodes[0] = Node(id: 0, kind: "Root", view: stack, container: stack)
    }

    func apply(_ ops: [[String: Any]]) {
        var rearrange: [Int] = []
        for op in ops {
            guard let id = op["id"] as? Int else { continue }
            switch op["op"] as? String ?? "" {
            case "create":
                let props = op["props"] as? [String: Any] ?? [:]
                let n = create(id: id, kind: op["kind"] as? String ?? "", props: props)
                n.events = op["events"] as? [String] ?? []
                nodes[id] = n
                setProps(n, props)
            case "update":
                guard let n = nodes[id] else { continue }
                let props = op["props"] as? [String: Any] ?? [:]
                setProps(n, props)
                if let events = op["events"] as? [String] { n.events = events }
                if props.keys.contains(where: { ["width", "height", "grow"].contains($0) }) { rearrange.append(n.parent) }
                if props.keys.contains(where: { ["gap", "align", "justify", "padding"].contains($0) }) { rearrange.append(id) }
            case "children":
                guard let p = nodes[id] else { continue }
                p.children = op["ids"] as? [Int] ?? []
                for c in p.children { nodes[c]?.parent = id }
                rearrange.append(id)
            case "remove":
                if let n = nodes.removeValue(forKey: id) { n.view.removeFromSuperview() }
            default:
                break
            }
        }
        for id in rearrange {
            if let n = nodes[id] { arrange(n) }
        }
    }

    // ---------- widgets ----------

    private func create(id: Int, kind: String, props: [String: Any]) -> Node {
        switch kind {
        case "Column", "Row":
            let s = UIStackView()
            s.axis = kind == "Row" ? .horizontal : .vertical
            s.alignment = kind == "Row" ? .center : .fill
            return Node(id: id, kind: kind, view: s, container: s)
        case "Scroll":
            let scroll = UIScrollView()
            let s = UIStackView()
            s.axis = .vertical
            s.translatesAutoresizingMaskIntoConstraints = false
            scroll.addSubview(s)
            NSLayoutConstraint.activate([
                s.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor),
                s.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor),
                s.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor),
                s.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor),
                s.widthAnchor.constraint(equalTo: scroll.frameLayoutGuide.widthAnchor),
            ])
            return Node(id: id, kind: kind, view: scroll, container: s)
        case "Button":
            let b = UIButton(type: .system)
            let n = Node(id: id, kind: kind, view: b, container: nil)
            let t = target(for: n)
            b.addTarget(t, action: #selector(EventTarget.press), for: .touchUpInside)
            return n
        case "Input":
            let f = UITextField()
            f.borderStyle = .roundedRect
            let n = Node(id: id, kind: kind, view: f, container: nil)
            let t = target(for: n)
            f.addTarget(t, action: #selector(EventTarget.changed(_:)), for: .editingChanged)
            f.delegate = t
            return n
        case "Toggle":
            let row = UIStackView()
            row.axis = .horizontal
            row.spacing = 8
            row.alignment = .center
            let toggle = UISwitch()
            let label = UILabel()
            row.addArrangedSubview(toggle)
            row.addArrangedSubview(label)
            let n = Node(id: id, kind: kind, view: row, container: nil)
            n.toggle = toggle
            n.label = label
            toggle.addTarget(target(for: n), action: #selector(EventTarget.toggled(_:)), for: .valueChanged)
            return n
        case "Image":
            let img = UIImageView()
            img.contentMode = .scaleAspectFit
            img.clipsToBounds = true
            return Node(id: id, kind: kind, view: img, container: nil)
        case "Text":
            let label = UILabel()
            label.numberOfLines = 0
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
        if props.keys.contains(where: { ["width", "height"].contains($0) }) { resize(n) }
    }

    private func setProp(_ n: Node, _ k: String, _ v: Any?) {
        let view = n.view
        switch k {
        case "text":
            let s = v.map { "\($0)" } ?? ""
            if let b = view as? UIButton { b.setTitle(s, for: .normal) }
            (n.label ?? view as? UILabel)?.text = s
        case "value":
            if let toggle = n.toggle {
                toggle.setOn((v as? Bool) ?? false, animated: false)
            } else if let f = view as? UITextField {
                let s = v.map { "\($0)" } ?? ""
                if f.text != s { f.text = s }
            }
        case "placeholder":
            (view as? UITextField)?.placeholder = v.map { "\($0)" }
        case "disabled":
            let enabled = !((v as? Bool) ?? false)
            (view as? UIControl)?.isEnabled = enabled
            n.toggle?.isEnabled = enabled
        case "secure":
            (view as? UITextField)?.isSecureTextEntry = (v as? Bool) ?? false
        case "lines":
            (view as? UILabel)?.numberOfLines = (v as? Int) ?? 0
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
            (n.label ?? view as? UILabel)?.textColor = c ?? .label
            (view as? UITextField)?.textColor = c ?? .label
            if let b = view as? UIButton { b.setTitleColor(c, for: .normal) }
        case "padding":
            if let s = n.container {
                let p = CGFloat((v as? Double) ?? 0)
                s.isLayoutMarginsRelativeArrangement = p > 0
                s.layoutMargins = UIEdgeInsets(top: p, left: p, bottom: p, right: p)
            }
        case "gap":
            n.container?.spacing = CGFloat((v as? Double) ?? 0)
        default:
            break // layout, box and font props are applied together below and in arrange()
        }
    }

    private func refont(_ n: Node) {
        let size = CGFloat((n.props["size"] as? Double) ?? Double(UIFont.labelFontSize))
        let w = n.props["weight"]
        let bold = (w as? String).map { $0 == "bold" || $0 == "bolder" } ?? (((w as? Double) ?? 400) >= 600)
        let font = UIFont.systemFont(ofSize: size, weight: bold ? .bold : .regular)
        (n.label ?? n.view as? UILabel)?.font = font
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

    private func resize(_ n: Node) {
        NSLayoutConstraint.deactivate(n.size)
        n.size = []
        if let w = n.props["width"] as? Double { n.size.append(n.view.widthAnchor.constraint(equalToConstant: CGFloat(w))) }
        if let h = n.props["height"] as? Double { n.size.append(n.view.heightAnchor.constraint(equalToConstant: CGFloat(h))) }
        NSLayoutConstraint.activate(n.size)
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

    // ---------- layout ----------

    /// Puts a node's children in order and sets how they share the stack's main axis.
    private func arrange(_ p: Node) {
        guard let stack = p.container else { return }
        let vertical = stack.axis == .vertical
        switch p.props["align"] as? String {
        case "start": stack.alignment = vertical ? .leading : .top
        case "center": stack.alignment = .center
        case "end": stack.alignment = vertical ? .trailing : .bottom
        case "stretch": stack.alignment = .fill
        default: stack.alignment = vertical ? .fill : .center
        }
        switch p.props["justify"] as? String {
        case "between": stack.distribution = .equalSpacing
        case "around": stack.distribution = .equalCentering
        default: stack.distribution = .fill
        }
        var want = p.children.compactMap { nodes[$0]?.view }
        if stack.distribution == .fill { want.append(p.filler) }
        for v in stack.arrangedSubviews where !want.contains(v) {
            stack.removeArrangedSubview(v)
            v.removeFromSuperview()
        }
        for (i, v) in want.enumerated() where stack.arrangedSubviews.firstIndex(of: v) != i {
            stack.insertArrangedSubview(v, at: i)
        }
        // Growing children take free space first, then the filler; others keep their size.
        let axis: NSLayoutConstraint.Axis = vertical ? .vertical : .horizontal
        for id in p.children {
            guard let c = nodes[id] else { continue }
            let grow = ((c.props["grow"] as? Double) ?? (c.kind == "Spacer" ? 1 : 0)) > 0
            c.view.setContentHuggingPriority(UILayoutPriority(grow ? 1 : 750), for: axis)
            c.view.setContentCompressionResistancePriority(UILayoutPriority(grow ? 250 : 750), for: axis)
        }
        p.filler.setContentHuggingPriority(UILayoutPriority(2), for: axis)
    }

    // ---------- automation ----------

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
        guard let window = nodes[0]?.view.window else { return nil }
        let renderer = UIGraphicsImageRenderer(bounds: window.bounds)
        let image = renderer.image { _ in window.drawHierarchy(in: window.bounds, afterScreenUpdates: true) }
        return image.pngData()
    }
}
