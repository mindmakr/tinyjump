import AppKit
import Foundation

/// Top-left flipped container matching tj-core's layout coordinate system.
class FlippedView: NSView {
    override var isFlipped: Bool { true }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
    }

    required init?(coder: NSCoder) {
        fatalError("not used from storyboards")
    }
}

/// Turns AppKit control events into runtime UI events for one node.
final class EventTarget: NSObject, NSTextFieldDelegate {
    let fire: (String, Any) -> Void
    var onScroll: ((NSScrollView) -> Void)?
    weak var scrollView: NSScrollView?

    init(fire: @escaping (String, Any) -> Void) {
        self.fire = fire
    }

    @objc func press() { fire("onPress", NSNull()) }

    @objc func toggled(_ sender: NSSwitch) {
        fire("onChange", sender.state == .on)
    }

    @objc func scrollChanged(_ note: Notification) {
        if let sv = scrollView { onScroll?(sv) }
    }

    func controlTextDidChange(_ obj: Notification) {
        if let f = obj.object as? NSTextField {
            fire("onChange", f.stringValue)
        }
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        if commandSelector == #selector(NSResponder.insertNewline(_:)), let f = control as? NSTextField {
            fire("onSubmit", f.stringValue)
            return true
        }
        return false
    }
}

/// Wraps a non-editable wrapping NSTextField with explicit padding insets.
final class PaddedLabel: FlippedView {
    let field = NSTextField(wrappingLabelWithString: "")
    var insets = NSEdgeInsetsZero {
        didSet { needsLayout = true }
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        field.drawsBackground = false
        field.isBezeled = false
        field.isEditable = false
        field.isSelectable = false
        field.lineBreakMode = .byWordWrapping
        field.textColor = .labelColor
        addSubview(field)
    }

    required init?(coder: NSCoder) {
        fatalError("not used from storyboards")
    }

    override func layout() {
        super.layout()
        let w = max(0, bounds.width - insets.left - insets.right)
        let h = max(0, bounds.height - insets.top - insets.bottom)
        field.frame = NSRect(x: insets.left, y: insets.top, width: w, height: h)
    }

    func measure(maxWidth: CGFloat?) -> CGSize {
        let innerMax = maxWidth.map { max(1, $0 - insets.left - insets.right) } ?? CGFloat.greatestFiniteMagnitude
        let raw = field.cell?.cellSize(forBounds: NSRect(x: 0, y: 0, width: innerMax, height: .greatestFiniteMagnitude)) ?? .zero
        return CGSize(width: raw.width + insets.left + insets.right, height: raw.height + insets.top + insets.bottom)
    }
}

/// Wraps NSTextField / NSSecureTextField so `secure` can toggle without recreating the node.
final class InputView: FlippedView {
    private(set) var field: NSTextField
    private var isSecure = false
    weak var eventDelegate: NSTextFieldDelegate? {
        didSet { field.delegate = eventDelegate }
    }

    override init(frame: NSRect) {
        field = NSTextField(string: "")
        super.init(frame: frame)
        configure(field)
        addSubview(field)
    }

    required init?(coder: NSCoder) {
        fatalError("not used from storyboards")
    }

    private func configure(_ f: NSTextField) {
        f.isBezeled = true
        f.bezelStyle = .roundedBezel
        f.isEditable = true
        f.isSelectable = true
        f.usesSingleLineMode = true
        f.cell?.wraps = false
        f.cell?.isScrollable = true
        f.delegate = eventDelegate
    }

    func setSecure(_ secure: Bool) {
        guard secure != isSecure else { return }
        isSecure = secure
        let prev = field
        let next: NSTextField = secure ? NSSecureTextField(string: prev.stringValue) : NSTextField(string: prev.stringValue)
        configure(next)
        next.placeholderString = prev.placeholderString
        next.font = prev.font
        next.textColor = prev.textColor
        next.alignment = prev.alignment
        next.userInterfaceLayoutDirection = prev.userInterfaceLayoutDirection
        next.isEnabled = prev.isEnabled
        prev.delegate = nil
        prev.removeFromSuperview()
        field = next
        addSubview(next)
        needsLayout = true
    }

    override func layout() {
        super.layout()
        field.frame = bounds
    }

    func measure(maxWidth: CGFloat?) -> CGSize {
        let s = field.fittingSize
        let w = max(120, s.width)
        return CGSize(width: maxWidth.map { min(w, $0) } ?? w, height: max(28, s.height))
    }
}

/// Native macOS segmented tab bar (`<Tabs>`).
final class TabsView: FlippedView {
    private let control = NSSegmentedControl(labels: [], trackingMode: .selectOne, target: nil, action: nil)
    private var values: [String] = []
    var onSelect: ((String) -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        control.target = self
        control.action = #selector(changed)
        addSubview(control)
    }

    required init?(coder: NSCoder) {
        fatalError("not used from storyboards")
    }

    func setItems(_ items: [Any], selected: String?) {
        values = []
        var labels: [String] = []
        for raw in items {
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
            labels.append(label)
        }
        control.segmentCount = labels.count
        for (i, lbl) in labels.enumerated() {
            control.setLabel(lbl, forSegment: i)
        }
        setSelected(selected)
        needsLayout = true
    }

    func setSelected(_ selected: String?) {
        if let s = selected, let idx = values.firstIndex(of: s) {
            control.selectedSegment = idx
        } else {
            control.selectedSegment = -1
        }
    }

    @objc private func changed() {
        let idx = control.selectedSegment
        if idx >= 0 && idx < values.count {
            onSelect?(values[idx])
        }
    }

    override func layout() {
        super.layout()
        control.frame = bounds
    }

    func measure(maxWidth: CGFloat?) -> CGSize {
        let s = control.fittingSize
        let w = max(44, s.width)
        return CGSize(width: maxWidth.map { min(w, $0) } ?? w, height: max(32, s.height))
    }
}

/// Vector icon backed by macOS 11+ SF Symbols (`<Icon>`).
final class IconView: NSImageView {
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

    override init(frame: NSRect) {
        super.init(frame: frame)
        imageScaling = .scaleProportionallyUpOrDown
        contentTintColor = .labelColor
    }

    required init?(coder: NSCoder) {
        fatalError("not used from storyboards")
    }

    func setIcon(_ name: String) {
        let sym = IconView.symbols[name] ?? "circle"
        image = NSImage(systemSymbolName: sym, accessibilityDescription: nil)
    }
}

/// Native macOS switch with a label beside it (`<Toggle>`).
final class ToggleView: FlippedView {
    let toggle = NSSwitch()
    let label = NSTextField(wrappingLabelWithString: "")
    private let spacing: CGFloat = 8

    override init(frame: NSRect) {
        super.init(frame: frame)
        label.textColor = .labelColor
        addSubview(toggle)
        addSubview(label)
    }

    required init?(coder: NSCoder) {
        fatalError("not used from storyboards")
    }

    func measure(maxWidth: CGFloat?) -> CGSize {
        let sw = toggle.fittingSize
        let innerW = maxWidth.map { max(1, $0 - sw.width - spacing) } ?? CGFloat.greatestFiniteMagnitude
        let l = label.cell?.cellSize(forBounds: NSRect(x: 0, y: 0, width: innerW, height: .greatestFiniteMagnitude)) ?? .zero
        return CGSize(width: sw.width + spacing + l.width, height: max(sw.height, l.height))
    }

    override func layout() {
        super.layout()
        let sw = toggle.fittingSize
        let labelWidth = max(0, bounds.width - sw.width - spacing)
        let rtl = userInterfaceLayoutDirection == .rightToLeft
        let measured = label.cell?.cellSize(forBounds: NSRect(x: 0, y: 0, width: max(1, labelWidth), height: .greatestFiniteMagnitude)).height ?? bounds.height
        let lh = min(bounds.height, ceil(measured))
        toggle.frame = NSRect(x: rtl ? bounds.width - sw.width : 0, y: (bounds.height - sw.height) / 2, width: sw.width, height: sw.height)
        label.frame = NSRect(x: rtl ? 0 : sw.width + spacing, y: (bounds.height - lh) / 2, width: labelWidth, height: lh)
    }
}

/// The app's root view: notifies on resize so tj-core recomputes frames.
final class RootView: FlippedView {
    var onResize: (() -> Void)?
    private var last = CGSize.zero

    override func layout() {
        super.layout()
        if bounds.size != last {
            last = bounds.size
            onResize?()
        }
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        if newSize != last {
            last = newSize
            onResize?()
        }
    }
}

/// Applies view-tree patches to native AppKit views and places them at the frames
/// computed by tj-core (`crates/tj-core/src/ui/layout.rs`).
final class Renderer {
    final class Node {
        let id: Int
        let kind: String
        let view: NSView
        let container: NSView?
        var target: EventTarget?
        var nativeComponent: NativeComponent?
        var scrollObserver: NSObjectProtocol?
        var imageRequestId: Int = 0
        var lastScrollExtent: CGFloat = -1
        var children: [Node] = []
        var props: [String: Any] = [:]
        var events: [String] = []
        var quiet = false

        init(id: Int, kind: String, view: NSView, container: NSView?) {
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

    init(host: NSView) {
        root.userInterfaceLayoutDirection = host.userInterfaceLayoutDirection
        root.autoresizingMask = [.width, .height]
        root.frame = host.bounds
        host.addSubview(root)
        nodes[0] = Node(id: 0, kind: "Root", view: root, container: root)
    }

    func setLocale(_ tag: String) {
        let trimmed = tag.trimmingCharacters(in: .whitespacesAndNewlines)
        localeId = trimmed.isEmpty ? Locale.current.identifier.replacingOccurrences(of: "_", with: "-") : trimmed
        let isRtl = Renderer.isRtlLocale(localeId)
        root.userInterfaceLayoutDirection = isRtl ? .rightToLeft : .leftToRight
        inheritDirection(root)
        for n in nodes.values {
            if n.kind == "Format" {
                (n.view as? PaddedLabel)?.field.stringValue = formatNumber(n.props)
            } else if n.kind == "Time" {
                (n.view as? PaddedLabel)?.field.stringValue = Times.show(n.props, locale: localeId)
            } else if let input = n.view as? InputView {
                input.field.alignment = isRtl ? .right : .left
            }
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
                let childNodes = (op["ids"] as? [Int] ?? []).compactMap { nodes[$0] }
                let views = childNodes.map { $0.view }
                if parent.kind == "List", views.count < box.subviews.count { parent.lastScrollExtent = -1 }
                parent.children = childNodes
                if box.subviews != views { box.subviews = views }
            case "remove":
                if let n = nodes.removeValue(forKey: id) {
                    if let obs = n.scrollObserver { NotificationCenter.default.removeObserver(obs) }
                    n.nativeComponent?.destroy?(n.view)
                    n.view.removeFromSuperview()
                }
            default:
                break
            }
        }
        return !ops.isEmpty
    }

    func destroy() {
        for n in nodes.values {
            if let obs = n.scrollObserver { NotificationCenter.default.removeObserver(obs) }
            n.nativeComponent?.destroy?(n.view)
        }
        nodes.removeAll()
    }

    private func inheritDirection(_ view: NSView) {
        view.userInterfaceLayoutDirection = root.userInterfaceLayoutDirection
        view.subviews.forEach(inheritDirection)
    }

    var rtl: Bool { root.userInterfaceLayoutDirection == .rightToLeft }

    func frames(_ list: [[String: Any]]) {
        for f in list {
            guard let id = f["id"] as? Int, let n = nodes[id] else { continue }
            let num = { (k: String) in CGFloat((f[k] as? Double) ?? 0) }
            n.view.frame = NSRect(x: num("x"), y: num("y"), width: num("w"), height: num("h"))
            if let content = f["content"] as? Double, let scroll = n.view as? NSScrollView, let box = n.container {
                let horiz = n.kind == "List" && ((n.props["horizontal"] as? Bool) ?? false)
                scroll.hasHorizontalScroller = horiz
                scroll.hasVerticalScroller = !horiz
                box.frame = horiz
                    ? NSRect(x: 0, y: 0, width: max(num("w"), CGFloat(content)), height: num("h"))
                    : NSRect(x: 0, y: 0, width: num("w"), height: max(num("h"), CGFloat(content)))
            }
        }
        root.layoutSubtreeIfNeeded()
    }

    func measure(node: Int, maxWidth: CGFloat?) -> CGSize {
        guard let n = nodes[node] else { return .zero }
        let s: CGSize
        switch n.view {
        case let l as PaddedLabel:
            s = l.measure(maxWidth: maxWidth)
        case let i as InputView:
            s = i.measure(maxWidth: maxWidth)
        case let t as ToggleView:
            s = t.measure(maxWidth: maxWidth)
        case let tabs as TabsView:
            s = tabs.measure(maxWidth: maxWidth)
        case let icon as IconView:
            s = CGSize(width: icon.iconSize, height: icon.iconSize)
        case let b as NSButton:
            let pad = CGFloat((n.props["padding"] as? Double) ?? 0)
            let fit = b.fittingSize
            s = CGSize(width: fit.width + pad * 2, height: max(32, fit.height + pad * 2))
        default:
            s = n.view.fittingSize
        }
        let width = maxWidth.map { min(s.width, $0) } ?? s.width
        return CGSize(width: ceil(max(0, width)) + 1, height: ceil(max(0, s.height)))
    }

    func viewportSize() -> CGSize? {
        root.bounds.width > 0 && root.bounds.height > 0 ? root.bounds.size : nil
    }

    // ---------- widgets ----------

    private func create(id: Int, kind: String, props: [String: Any]) -> Node {
        switch kind {
        case "Column", "Row", "Theme":
            let box = FlippedView()
            return Node(id: id, kind: kind, view: box, container: box)
        case "Scroll":
            let scroll = NSScrollView()
            scroll.drawsBackground = false
            scroll.borderType = .noBorder
            scroll.hasVerticalScroller = true
            scroll.autohidesScrollers = true
            let content = FlippedView()
            scroll.documentView = content
            return Node(id: id, kind: kind, view: scroll, container: content)
        case "List":
            let scroll = NSScrollView()
            scroll.drawsBackground = false
            scroll.borderType = .noBorder
            scroll.hasVerticalScroller = true
            scroll.autohidesScrollers = true
            let content = FlippedView()
            scroll.documentView = content
            scroll.contentView.postsBoundsChangedNotifications = true
            let n = Node(id: id, kind: kind, view: scroll, container: content)
            let t = target(for: n)
            t.scrollView = scroll
            t.onScroll = { [weak n, weak t] sv in
                guard let n = n, let t = t, let doc = sv.documentView else { return }
                let horiz = (n.props["horizontal"] as? Bool) ?? false
                let extent = horiz ? doc.bounds.width : doc.bounds.height
                let viewSize = horiz ? sv.contentView.bounds.width : sv.contentView.bounds.height
                let offset = horiz ? sv.contentView.bounds.origin.x : sv.contentView.bounds.origin.y
                if extent < n.lastScrollExtent { n.lastScrollExtent = -1 }
                if extent > viewSize, extent - offset - viewSize <= 120, n.lastScrollExtent != extent {
                    n.lastScrollExtent = extent
                    t.fire("onEndReached", NSNull())
                }
            }
            n.scrollObserver = NotificationCenter.default.addObserver(
                forName: NSView.boundsDidChangeNotification,
                object: scroll.contentView,
                queue: .main
            ) { [weak t] note in
                t?.scrollChanged(note)
            }
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
            let b = NSButton(title: "", target: nil, action: nil)
            b.bezelStyle = .rounded
            let n = Node(id: id, kind: kind, view: b, container: nil)
            let t = target(for: n)
            b.target = t
            b.action = #selector(EventTarget.press)
            return n
        case "Input":
            let v = InputView()
            v.field.alignment = rtl ? .right : .left
            let n = Node(id: id, kind: kind, view: v, container: nil)
            v.eventDelegate = target(for: n)
            return n
        case "Toggle":
            let v = ToggleView()
            let n = Node(id: id, kind: kind, view: v, container: nil)
            let t = target(for: n)
            v.toggle.target = t
            v.toggle.action = #selector(EventTarget.toggled(_:))
            return n
        case "Image":
            let img = NSImageView()
            img.imageScaling = .scaleProportionallyUpOrDown
            img.wantsLayer = true
            return Node(id: id, kind: kind, view: img, container: nil)
        case "Text", "Time", "Format":
            return Node(id: id, kind: kind, view: PaddedLabel(), container: nil)
        case "Spacer":
            return Node(id: id, kind: kind, view: FlippedView(), container: nil)
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
            missing.field.stringValue = "<\(kind)>"
            return Node(id: id, kind: kind, view: missing, container: nil)
        }
    }

    private func target(for n: Node) -> EventTarget {
        let t = EventTarget { [weak self, weak n] event, value in
            guard let self = self, let n = n, !n.quiet, n.events.contains(event) else { return }
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
                if k == "id" { n.view.setAccessibilityIdentifier(none ? nil : (v as? String)) }
                else if k == "label" { n.view.setAccessibilityLabel(none ? nil : "\(v)") }
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
        if n.kind == "Theme", props.keys.contains(where: { ["mode"].contains($0) }) { applyTheme(n) }
        if props.keys.contains(where: { ["background", "radius", "border"].contains($0) || (n.kind == "Theme" && ["surface", "mode"].contains($0)) }) { restyle(n) }
        if n.nativeComponent == nil, props.keys.contains(where: { ["size", "weight"].contains($0) }) { refont(n) }
    }

    private func applyTheme(_ n: Node) {
        if let mode = n.props["mode"] as? String {
            n.view.appearance = mode == "dark" ? NSAppearance(named: .darkAqua) : mode == "light" ? NSAppearance(named: .aqua) : nil
        } else {
            n.view.appearance = nil
        }
    }

    private func setProp(_ n: Node, _ k: String, _ v: Any?) {
        let view = n.view
        let label = (view as? PaddedLabel)?.field ?? (view as? ToggleView)?.label
        if let icon = view as? IconView {
            if k == "name" { icon.setIcon(v.map { "\($0)" } ?? ""); return }
            if k == "size" { icon.iconSize = CGFloat((v as? Double) ?? 20); return }
            if k == "color" { icon.contentTintColor = v.flatMap { Renderer.color("\($0)") } ?? .labelColor; return }
        }
        if n.kind == "Time", ["value", "show", "style", "zone"].contains(k) {
            label?.stringValue = Times.show(n.props, locale: localeId)
            return
        }
        if n.kind == "Format", ["value", "kind", "currency", "decimals", "locale"].contains(k) {
            label?.stringValue = formatNumber(n.props)
            return
        }
        if let input = view as? InputView, k == "type" || k == "zone" || (k == "value" && Times.typed(n.props["type"] as? String)) {
            let type = n.props["type"] as? String ?? ""
            let zone = n.props["zone"] as? String
            let s = Times.typed(type) ? Times.text(type, zone, n.props["value"]) : (n.props["value"].map { "\($0)" } ?? "")
            if input.field.stringValue != s { input.field.stringValue = s }
            return
        }
        switch k {
        case "text":
            let s = v.map { "\($0)" } ?? ""
            if let b = view as? NSButton { b.title = s }
            label?.stringValue = s
        case "value":
            if let t = view as? ToggleView {
                t.toggle.state = ((v as? Bool) ?? false) ? .on : .off
            } else if let input = view as? InputView {
                let s = v.map { "\($0)" } ?? ""
                if input.field.stringValue != s { input.field.stringValue = s }
            }
        case "placeholder":
            (view as? InputView)?.field.placeholderString = v.map { "\($0)" }
        case "disabled":
            let enabled = !((v as? Bool) ?? false)
            (view as? NSControl)?.isEnabled = enabled
            (view as? InputView)?.field.isEnabled = enabled
            (view as? ToggleView)?.toggle.isEnabled = enabled
        case "secure":
            (view as? InputView)?.setSecure((v as? Bool) ?? false)
        case "lines":
            label?.maximumNumberOfLines = (v as? Int) ?? 0
        case "src":
            if let img = view as? NSImageView { load(n, img, v as? String) }
        case "fit":
            let fit = v as? String
            (view as? NSImageView)?.imageScaling = fit == "fill" ? .scaleAxesIndependently : .scaleProportionallyUpOrDown
        case "id":
            view.setAccessibilityIdentifier(v as? String)
        case "label":
            view.setAccessibilityLabel(v.map { "\($0)" })
        case "color":
            let c = v.flatMap { Renderer.color("\($0)") }
            label?.textColor = c ?? .labelColor
            (view as? InputView)?.field.textColor = c ?? .labelColor
            if let b = view as? NSButton { b.contentTintColor = c }
        case "padding":
            let p = CGFloat((v as? Double) ?? 0)
            (view as? PaddedLabel)?.insets = NSEdgeInsets(top: p, left: p, bottom: p, right: p)
        default:
            break
        }
    }

    private func refont(_ n: Node) {
        let size = CGFloat((n.props["size"] as? Double) ?? Double(NSFont.systemFontSize))
        let w = n.props["weight"]
        let bold = (w as? String).map { $0 == "bold" || $0 == "bolder" } ?? (((w as? Double) ?? 400) >= 600)
        let font = NSFont.systemFont(ofSize: size, weight: bold ? .bold : .regular)
        ((n.view as? PaddedLabel)?.field ?? (n.view as? ToggleView)?.label)?.font = font
        (n.view as? InputView)?.field.font = font
        (n.view as? NSButton)?.font = font
    }

    private func restyle(_ n: Node) {
        n.view.wantsLayer = true
        var bg = (n.props["background"] as? String).flatMap(Renderer.color)
        if bg == nil && n.kind == "Theme" {
            if let surface = n.props["surface"] as? String, let c = Renderer.color(surface) {
                bg = c
            } else if n.props["mode"] != nil {
                bg = .windowBackgroundColor
            }
        }
        n.view.effectiveAppearance.performAsCurrentDrawingAppearance {
            if n.nativeComponent == nil || n.props["background"] != nil {
                n.view.layer?.backgroundColor = bg?.cgColor
            }
            let radius = CGFloat((n.props["radius"] as? Double) ?? 0)
            n.view.layer?.cornerRadius = radius
            n.view.layer?.masksToBounds = radius > 0 || n.view is NSImageView
            switch n.props["border"] ?? NSNull() {
            case let width as Double:
                n.view.layer?.borderWidth = CGFloat(width)
                n.view.layer?.borderColor = NSColor.labelColor.cgColor
            case let spec as String:
                var width: CGFloat = 1
                var color = NSColor.labelColor
                for part in spec.split(separator: " ") {
                    if part.hasSuffix("px"), let w = Double(part.dropLast(2)) { width = CGFloat(w) }
                    if let c = Renderer.color(String(part)) { color = c }
                }
                n.view.layer?.borderWidth = width
                n.view.layer?.borderColor = color.cgColor
            default:
                n.view.layer?.borderWidth = 0
            }
        }
    }

    private func load(_ node: Node, _ img: NSImageView, _ src: String?) {
        node.imageRequestId += 1
        let reqId = node.imageRequestId
        img.image = nil
        guard let src = src, src.hasPrefix("https://") || src.hasPrefix("http://"), let url = URL(string: src) else { return }
        URLSession.shared.dataTask(with: url) { data, _, _ in
            guard let data = data, let image = NSImage(data: data) else { return }
            DispatchQueue.main.async {
                if node.imageRequestId == reqId { img.image = image }
            }
        }.resume()
    }

    static func color(_ s: String) -> NSColor? {
        let names: [String: NSColor] = [
            "black": .black, "white": .white, "red": .systemRed, "green": .systemGreen,
            "blue": .systemBlue, "gray": .systemGray, "grey": .systemGray, "transparent": .clear,
        ]
        if let named = names[s.lowercased()] { return named }
        guard s.hasPrefix("#") else { return nil }
        var hex = String(s.dropFirst())
        if hex.count == 3 { hex = hex.map { "\($0)\($0)" }.joined() }
        if hex.count == 6 { hex += "ff" }
        guard hex.count == 8, let v = UInt64(hex, radix: 16) else { return nil }
        return NSColor(
            srgbRed: CGFloat((v >> 24) & 0xff) / 255,
            green: CGFloat((v >> 16) & 0xff) / 255,
            blue: CGFloat((v >> 8) & 0xff) / 255,
            alpha: CGFloat(v & 0xff) / 255
        )
    }

    // ---------- automation ----------

    func viewport() -> [String: Int] {
        let size = root.bounds.size
        return ["w": Int(size.width.rounded()), "h": Int(size.height.rounded())]
    }

    func forAgent(_ input: [String: Any]) -> [String: Any] {
        guard input["type"] as? String == "ui", let text = input["value"] as? String, let target = input["target"] as? [String: Any] else { return input }
        let byText: (String) -> Node? = { t in self.nodes.values.first { $0.props["label"] as? String == t || $0.props["placeholder"] as? String == t } }
        let node: Node? = (target["node"] as? Int).flatMap { nodes[$0] } ?? (target["id"] as? String).flatMap { id in nodes.values.first { $0.props["id"] as? String == id } } ?? (target["text"] as? String).flatMap(byText)
        guard let n = node, n.kind == "Input", let type = n.props["type"] as? String, Times.typed(type) else { return input }
        var out = input
        out["value"] = Times.value(type, n.props["zone"] as? String, text)
        return out
    }

    func addBounds(_ tree: [[String: Any]]) -> [[String: Any]] {
        let screen = root.bounds
        return tree.map { node in
            var n = node
            if let id = n["id"] as? Int, let shown = nodes[id], shown.kind == "Time" || shown.kind == "Format", var props = n["props"] as? [String: Any] {
                props["text"] = (shown.view as? PaddedLabel)?.field.stringValue ?? ""
                n["props"] = props
            }
            if let id = n["id"] as? Int, let v = nodes[id]?.view {
                let frame = v.convert(v.bounds, to: root)
                let visible = v.window != nil && !v.isHidden && frame.width > 0 && frame.height > 0 && frame.intersects(screen)
                n["bounds"] = ["x": Int(frame.minX.rounded()), "y": Int(frame.minY.rounded()), "w": Int(frame.width.rounded()), "h": Int(frame.height.rounded())]
                n["visible"] = visible
            }
            n["children"] = addBounds(n["children"] as? [[String: Any]] ?? [])
            return n
        }
    }

    /// Captures the root AppKit view hierarchy as PNG bytes without requiring screen-recording permissions.
    func screenshot() -> Data? {
        root.layoutSubtreeIfNeeded()
        guard root.bounds.width > 0, root.bounds.height > 0,
              let rep = root.bitmapImageRepForCachingDisplay(in: root.bounds) else { return nil }
        root.cacheDisplay(in: root.bounds, to: rep)
        return rep.representation(using: .png, properties: [:])
    }
}
