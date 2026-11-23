import Foundation

/// Times for `<Time>` and `<Input type="date|time|datetime|number">`, formatted and parsed
/// by Foundation's DateFormatter and TimeZone (zero bundled ICU data).
enum Times {
    static func typed(_ type: String?) -> Bool {
        ["number", "date", "time", "datetime"].contains(type ?? "")
    }

    static func zone(_ zone: String?) -> TimeZone {
        zone == "UTC" ? TimeZone(identifier: "UTC")! : TimeZone.current
    }

    private static func iso(_ type: String, _ tz: TimeZone) -> DateFormatter {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = type == "time" ? TimeZone(identifier: "UTC")! : tz
        f.isLenient = false
        f.dateFormat = type == "date" ? "yyyy-MM-dd" : type == "time" ? "HH:mm" : "yyyy-MM-dd HH:mm"
        return f
    }

    static func show(_ props: [String: Any], locale localeId: String? = nil) -> String {
        guard let ms = (props["value"] as? NSNumber)?.doubleValue, ms.isFinite else { return "" }
        let date = Date(timeIntervalSince1970: ms / 1000)
        let show = props["show"] as? String ?? "datetime"
        let tz = zone(props["zone"] as? String)
        if props["style"] as? String == "iso" {
            let f = iso(show == "date" ? "date" : "datetime", tz)
            let s = f.string(from: date)
            return show == "date" ? s : show == "time" ? String(s.suffix(5)) : s
        }
        let f = DateFormatter()
        if let raw = localeId, !raw.isEmpty {
            let tag = raw.lowercased() == "pseudo" ? "en-US" : raw
            f.locale = Locale(identifier: tag)
        }
        f.timeZone = tz
        f.dateStyle = show == "time" ? .none : .medium
        f.timeStyle = show == "date" ? .none : .short
        return f.string(from: date)
    }

    static func value(_ type: String, _ zone: String?, _ text: String) -> Any {
        let v = text.trimmingCharacters(in: .whitespaces).replacingOccurrences(of: "T", with: " ")
        if v.isEmpty { return NSNull() }
        if type == "number" {
            guard let d = Double(v), d.isFinite else { return NSNull() }
            return d
        }
        guard let date = iso(type, self.zone(zone)).date(from: v) else { return NSNull() }
        return (date.timeIntervalSince1970 * 1000).rounded()
    }

    static func text(_ type: String, _ zone: String?, _ value: Any?) -> String {
        guard let ms = (value as? NSNumber)?.doubleValue, ms.isFinite else { return "" }
        if type == "number" { return ms == ms.rounded() && abs(ms) < 1e15 ? String(Int64(ms)) : String(ms) }
        return iso(type, self.zone(zone)).string(from: Date(timeIntervalSince1970: ms / 1000))
    }
}
