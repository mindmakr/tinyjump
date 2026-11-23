import UIKit

/// Turns UIKit control events into runtime UI events for one node.
final class EventTarget: NSObject, UITextFieldDelegate, UIScrollViewDelegate {
    let fire: (String, Any) -> Void
    var onScroll: ((UIScrollView) -> Void)?

    init(fire: @escaping (String, Any) -> Void) {
        self.fire = fire
    }

    @objc func press() { fire("onPress", NSNull()) }
    @objc func changed(_ sender: UITextField) { fire("onChange", sender.text ?? "") }
    @objc func toggled(_ sender: UISwitch) { fire("onChange", sender.isOn) }
    @objc func picked(_ sender: UIDatePicker) { fire("onChange", Times.picked(sender)) }

    func textFieldShouldReturn(_ textField: UITextField) -> Bool {
        fire("onSubmit", textField.text ?? "")
        return true
    }

    func scrollViewDidScroll(_ scrollView: UIScrollView) {
        onScroll?(scrollView)
    }
}

/// A native segmented tab bar (`<Tabs>`), sized to at least 44pt tall for touch targets.
final class TabsView: UIView {
    private let control = UISegmentedControl()
    private var values: [String] = []
    var onSelect: ((String) -> Void)?

    override init(frame: CGRect) {
        super.init(frame: frame)
        control.addTarget(self, action: #selector(changed), for: .valueChanged)
        addSubview(control)
    }

    required init?(coder: NSCoder) {
        fatalError("not used from storyboards")
    }

    func setItems(_ items: [Any], selected: String?) {
        control.removeAllSegments()
        values = []
        for (i, raw) in items.enumerated() {
            let val: String
            let label: String
            if let dict = raw as? [String: Any] {
                let rawVal = dict["value"].flatMap { $0 is NSNull ? nil : "\($0)" }
                let rawLbl = dict["label"].flatMap { $0 is NSNull ? nil : "\($0)" }
                val = rawVal ?? rawLbl ?? ""
                label = rawLbl ?? val
            } else if raw is NSNull {
                val = ""
                label = ""
            } else {
                val = "\(raw)"
                label = val
            }
            values.append(val)
            control.insertSegment(withTitle: label, at: i, animated: false)
        }
        setSelected(selected)
    }

    func setSelected(_ selected: String?) {
        if let s = selected, let idx = values.firstIndex(of: s) {
            control.selectedSegmentIndex = idx
        } else {
            control.selectedSegmentIndex = UISegmentedControl.noSegment
        }
    }

    func setMutedColor(_ color: UIColor?) {
        let attrs: [NSAttributedString.Key: Any] = color.map { [.foregroundColor: $0] } ?? [:]
        control.setTitleTextAttributes(attrs, for: .normal)
    }

    @objc private func changed() {
        let idx = control.selectedSegmentIndex
        if idx >= 0 && idx < values.count {
            onSelect?(values[idx])
        }
    }

    override func sizeThatFits(_ size: CGSize) -> CGSize {
        let s = control.sizeThatFits(size)
        return CGSize(width: max(44, s.width), height: max(44, s.height))
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        control.frame = bounds
    }
}

/// A vector icon backed by iOS 13+ SF Symbols (`<Icon>`).
final class IconView: UIImageView {
    private static let symbols: [String: String] = [
        "check": "checkmark",
        "plus": "plus",
        "close": "xmark",
        "search": "magnifyingglass",
        "home": "house",
        "user": "person",
        "settings": "gear",
        "back": "chevron.left",
        "next": "chevron.right",
        "edit": "pencil",
        "trash": "trash",
        "calendar": "calendar",
        "clock": "clock",
        "bell": "bell",
        "mail": "envelope",
        "star": "star",
        "heart": "heart",
        "info": "info.circle",
        "warning": "exclamationmark.triangle",
        "lock": "lock",
        "menu": "line.horizontal.3",
        "share": "square.and.arrow.up",
        "refresh": "arrow.clockwise",
        "download": "arrow.down.circle",
    ]

    var iconSize: CGFloat = 20

    override init(frame: CGRect) {
        super.init(frame: frame)
        contentMode = .scaleAspectFit
        tintColor = .label
    }

    required init?(coder: NSCoder) {
        fatalError("not used from storyboards")
    }

    func setIcon(_ name: String) {
        let sym = IconView.symbols[name] ?? "circle"
        image = UIImage(systemName: sym)
    }

    override func sizeThatFits(_ size: CGSize) -> CGSize {
        CGSize(width: iconSize, height: iconSize)
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
        var nativeComponent: NativeComponent?
        var lastScrollExtent: CGFloat = -1
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
    var localeId: String = Locale.current.identifier.replacingOccurrences(of: "_", with: "-")

    func setLocale(_ tag: String) {
        let trimmed = tag.trimmingCharacters(in: .whitespacesAndNewlines)
        localeId = trimmed.isEmpty ? Locale.current.identifier.replacingOccurrences(of: "_", with: "-") : trimmed
        let rtl = Renderer.isRtlLocale(localeId)
        root.semanticContentAttribute = rtl ? .forceRightToLeft : .forceLeftToRight
        inheritDirection(root)
        for n in nodes.values where n.kind == "Format" {
            (n.view as? UILabel)?.text = formatNumber(n.props)
        }
    }

    private static func isRtlLocale(_ tag: String) -> Bool {
        let lang = tag.lowercased().split { $0 == "-" || $0 == "_" }.first.map(String.init) ?? ""
        return ["ar", "he", "fa", "ur", "ps", "sd", "ug", "yi", "dv", "ku"].contains(lang)
    }

    private func formatNumber(_ props: [String: Any]) -> String {
        guard let num = props["value"] as? NSNumber, num.doubleValue.isFinite else { return "" }
        let rawLoc = (props["locale"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? localeId
        let tag = rawLoc.lowercased() == "pseudo" ? "en-US" : rawLoc
        let kind = props["kind"] as? String ?? "number"
        let nf = NumberFormatter()
        nf.locale = Locale(identifier: tag)
        switch kind {
        case "currency":
            nf.numberStyle = .currency
            nf.currencyCode = (props["currency"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? "USD"
        case "percent":
            nf.numberStyle = .percent
        default:
            nf.numberStyle = .decimal
        }
        if let dec = props["decimals"] as? NSNumber {
            let d = max(0, min(20, dec.intValue))
            nf.minimumFractionDigits = d
            nf.maximumFractionDigits = d
        }
        return nf.string(from: num) ?? "\(num)"
    }

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
                setProps(n, props, isCreate: true)
            case "update":
                guard let n = nodes[id] else { continue }
                if let events = op["events"] as? [String] { n.events = events }
                setProps(n, op["props"] as? [String: Any] ?? [:], isCreate: false)
            case "children":
                guard let parent = nodes[id], let box = parent.container else { continue }
                let views = (op["ids"] as? [Int] ?? []).compactMap { nodes[$0]?.view }
                if parent.kind == "List", views.count < box.subviews.count { parent.lastScrollExtent = -1 }
                for v in box.subviews where !views.contains(v) { v.removeFromSuperview() }
                for (i, v) in views.enumerated() where box.subviews.firstIndex(of: v) != i { box.insertSubview(v, at: i) }
            case "remove":
                if let n = nodes.removeValue(forKey: id) {
                    n.nativeComponent?.destroy?(n.view)
                    n.view.removeFromSuperview()
                }
            default:
                break
            }
        }
        return !ops.isEmpty
    }

    /// Cleans up mounted custom native views when `TinyJump.stop()` is called.
    func destroy() {
        for n in nodes.values {
            n.nativeComponent?.destroy?(n.view)
        }
        nodes.removeAll()
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
                let horiz = n.kind == "List" && ((n.props["horizontal"] as? Bool) ?? false)
                box.frame = horiz
                    ? CGRect(x: 0, y: 0, width: CGFloat(content), height: num("h"))
                    : CGRect(x: 0, y: 0, width: num("w"), height: CGFloat(content))
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
        case "Column", "Row", "Theme":
            let box = UIView()
            return Node(id: id, kind: kind, view: box, container: box)
        case "Scroll":
            let scroll = UIScrollView()
            let content = UIView()
            scroll.addSubview(content)
            return Node(id: id, kind: kind, view: scroll, container: content)
        case "List":
            let scroll = UIScrollView()
            let content = UIView()
            scroll.addSubview(content)
            let n = Node(id: id, kind: kind, view: scroll, container: content)
            let t = target(for: n)
            t.onScroll = { [weak n, weak t] sv in
                guard let n = n, let t = t else { return }
                let horiz = (n.props["horizontal"] as? Bool) ?? false
                let extent = horiz ? sv.contentSize.width : sv.contentSize.height
                let viewSize = horiz ? sv.bounds.width : sv.bounds.height
                let offset = horiz ? sv.contentOffset.x : sv.contentOffset.y
                if extent < n.lastScrollExtent { n.lastScrollExtent = -1 }
                if extent > viewSize, extent - offset - viewSize <= 120, n.lastScrollExtent != extent {
                    n.lastScrollExtent = extent
                    t.fire("onEndReached", NSNull())
                }
            }
            scroll.delegate = t
            return n
        case "Tabs":
            let tabs = TabsView()
            let n = Node(id: id, kind: kind, view: tabs, container: nil)
            let t = target(for: n)
            tabs.onSelect = { [weak t] val in t?.fire("onChange", val) }
            return n
        case "Icon":
            return Node(id: id, kind: kind, view: IconView(frame: .zero), container: nil)
        case "Button":
            let b = UIButton(type: .system)
            let n = Node(id: id, kind: kind, view: b, container: nil)
            b.addTarget(target(for: n), action: #selector(EventTarget.press), for: .touchUpInside)
            return n
        case "Input":
            let f = UITextField()
            f.borderStyle = .roundedRect
            // Start where the layout starts (right in RTL), like Android and the web. `.natural`
            // would follow the typed text's script instead.
            f.textAlignment = rtl ? .right : .left
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
        case "Text", "Time", "Format":
            let label = PaddedLabel()
            label.numberOfLines = 0
            label.textAlignment = .natural
            return Node(id: id, kind: kind, view: label, container: nil)
        case "Spacer":
            return Node(id: id, kind: kind, view: UIView(), container: nil)
        default:
            if let comp = TinyJump.nativeComponent(kind) {
                weak var holder: Node?
                let view = comp.create(props) { [weak self] event, value in
                    guard let self = self, let n = holder, !n.quiet, n.events.contains(event) else { return }
                    self.tj?.send(["type": "ui", "target": ["node": n.id], "event": event, "value": value ?? NSNull()])
                }
                let n = Node(id: id, kind: kind, view: view, container: comp.container?(view))
                n.nativeComponent = comp
                holder = n
                return n
            }
            let missing = PaddedLabel()
            missing.text = "<\(kind)>"
            return Node(id: id, kind: kind, view: missing, container: nil)
        }
    }

    private func target(for n: Node) -> EventTarget {
        let t = EventTarget { [weak self, weak n] event, value in
            guard let self = self, let n = n, !n.quiet, n.events.contains(event) else { return }
            // A number input's text is sent as its number (pickers send numbers already).
            var value = value
            if let s = value as? String, let type = n.props["type"] as? String, Times.typed(type) {
                value = Times.value(type, n.props["zone"] as? String, s)
            }
            self.tj?.send(["type": "ui", "target": ["node": n.id], "event": event, "value": value])
        }
        n.target = t
        return t
    }

    private func setProps(_ n: Node, _ props: [String: Any], isCreate: Bool) {
        n.quiet = true
        defer { n.quiet = false }
        for (k, v) in props {
            let none = v is NSNull
            if none { n.props.removeValue(forKey: k) } else { n.props[k] = v }
            if n.nativeComponent != nil {
                if k == "id" { n.view.accessibilityIdentifier = none ? nil : (v as? String) }
                else if k == "label" { n.view.accessibilityLabel = none ? nil : "\(v)" }
            } else {
                setProp(n, k, none ? nil : v)
            }
        }
        if let comp = n.nativeComponent, !isCreate {
            comp.update(n.view, n.props, props)
        }
        if n.kind == "List", props["horizontal"] != nil {
            n.lastScrollExtent = -1
        }
        if n.kind == "Tabs", let tabs = n.view as? TabsView {
            let selected = n.props["value"].flatMap { $0 is NSNull ? nil : "\($0)" }
            if isCreate || props["items"] != nil {
                tabs.setItems(n.props["items"] as? [Any] ?? [], selected: selected)
            } else if props["value"] != nil {
                tabs.setSelected(selected)
            }
        }
        if props.keys.contains(where: { ["background", "radius", "border"].contains($0) || (n.kind == "Theme" && ["surface", "mode"].contains($0)) }) { restyle(n) }
        if n.kind == "Theme", props.keys.contains(where: { ["mode", "accent", "muted"].contains($0) }) { applyTheme(n) }
        if n.nativeComponent == nil, props.keys.contains(where: { ["size", "weight"].contains($0) }) { refont(n) }
    }

    private func applyTheme(_ n: Node) {
        if let mode = n.props["mode"] as? String {
            n.view.overrideUserInterfaceStyle = mode == "dark" ? .dark : mode == "light" ? .light : .unspecified
        } else {
            n.view.overrideUserInterfaceStyle = .unspecified
        }
        n.view.tintColor = (n.props["accent"] as? String).flatMap(Renderer.color)
        let mutedColor = (n.props["muted"] as? String).flatMap(Renderer.color)
        applyMuted(n.view, color: mutedColor)
    }

    private func applyMuted(_ view: UIView, color: UIColor?) {
        if let tabs = view as? TabsView {
            tabs.setMutedColor(color)
        }
        for sub in view.subviews {
            applyMuted(sub, color: color)
        }
    }

    /// Visual props. Layout props (width, gap, align, grow, ...) are the core's job.
    private func setProp(_ n: Node, _ k: String, _ v: Any?) {
        let view = n.view
        let label = (view as? UILabel) ?? (view as? ToggleView)?.label
        if let icon = view as? IconView {
            if k == "name" { icon.setIcon(v.map { "\($0)" } ?? ""); return }
            if k == "size" { icon.iconSize = CGFloat((v as? Double) ?? 20); return }
            if k == "color" { icon.tintColor = v.flatMap { Renderer.color("\($0)") } ?? .label; return }
        }
        if n.kind == "Time", ["value", "show", "style", "zone"].contains(k) {
            label?.text = Times.show(n.props)
            return
        }
        if n.kind == "Format", ["value", "kind", "currency", "decimals", "locale"].contains(k) {
            label?.text = formatNumber(n.props)
            return
        }
        if let f = view as? UITextField, k == "type" || k == "zone" || (k == "value" && Times.typed(n.props["type"] as? String)) {
            typed(f, n)
            return
        }
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
            if view is IconView { view.isAccessibilityElement = (v != nil) }
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
        var bg = (n.props["background"] as? String).flatMap(Renderer.color)
        if bg == nil && n.kind == "Theme" {
            if let surface = n.props["surface"] as? String, let c = Renderer.color(surface) {
                bg = c
            } else if n.props["mode"] != nil {
                bg = .systemBackground
            }
        }
        if n.nativeComponent == nil || n.props["background"] != nil {
            n.view.backgroundColor = bg
        }
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

    /// Sets an input up for its `type`: a number keypad, or the system's date picker as its
    /// keyboard; and shows its value.
    private func typed(_ f: UITextField, _ n: Node) {
        let type = n.props["type"] as? String ?? ""
        let zone = n.props["zone"] as? String
        f.keyboardType = type == "number" ? .decimalPad : .default
        if ["date", "time", "datetime"].contains(type) {
            let p = Times.picker(type, zone, n.props["value"])
            if let t = n.target { p.addTarget(t, action: #selector(EventTarget.picked(_:)), for: .valueChanged) }
            f.inputView = p
        } else {
            f.inputView = nil
        }
        let s = Times.typed(type) ? Times.text(type, zone, n.props["value"]) : (n.props["value"].map { "\($0)" } ?? "")
        if f.text != s { f.text = s }
    }

    /// An agent typing into a date, time or number input: its text becomes the value, as a
    /// person's entry does (`POST /_tj/input`).
    func forAgent(_ input: [String: Any]) -> [String: Any] {
        guard input["type"] as? String == "ui", let text = input["value"] as? String, let target = input["target"] as? [String: Any] else { return input }
        // By node, id, or the text an agent sees on it: its label or placeholder.
        let byText: (String) -> Node? = { t in self.nodes.values.first { $0.props["label"] as? String == t || $0.props["placeholder"] as? String == t } }
        let node: Node? = (target["node"] as? Int).flatMap { nodes[$0] } ?? (target["id"] as? String).flatMap { id in nodes.values.first { $0.props["id"] as? String == id } } ?? (target["text"] as? String).flatMap(byText)
        guard let n = node, n.kind == "Input", let type = n.props["type"] as? String, Times.typed(type) else { return input }
        var out = input
        out["value"] = Times.value(type, n.props["zone"] as? String, text)
        return out
    }

    /// Adds bounds (points, on screen) and visibility to an inspect tree, like the other hosts.
    func addBounds(_ tree: [[String: Any]]) -> [[String: Any]] {
        tree.map { node in
            var n = node
            // A time or formatted number shows what the platform made of it, so agents read what people see.
            if let id = n["id"] as? Int, let shown = nodes[id], shown.kind == "Time" || shown.kind == "Format", var props = n["props"] as? [String: Any] {
                props["text"] = (shown.view as? UILabel)?.text ?? ""
                n["props"] = props
            }
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
