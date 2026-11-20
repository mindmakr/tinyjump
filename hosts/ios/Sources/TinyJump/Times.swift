import UIKit

/// Times for `<Time>` and `<Input type="date|time|datetime|number">`, done by the platform:
/// DateFormatter formats them in the viewer's language, TimeZone converts them, and
/// UIDatePicker chooses them. Values are milliseconds (UTC); a time of day is milliseconds
/// since midnight (docs/language.md).
enum Times {
    static func typed(_ type: String?) -> Bool {
        ["number", "date", "time", "datetime"].contains(type ?? "")
    }

    static func zone(_ zone: String?) -> TimeZone {
        zone == "UTC" ? TimeZone(identifier: "UTC")! : TimeZone.current
    }

    /// The fixed `YYYY-MM-DD HH:MM` shapes, in `tz`; not lenient, so 2026-02-30 is no date.
    private static func iso(_ type: String, _ tz: TimeZone) -> DateFormatter {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = type == "time" ? TimeZone(identifier: "UTC")! : tz
        f.isLenient = false
        f.dateFormat = type == "date" ? "yyyy-MM-dd" : type == "time" ? "HH:mm" : "yyyy-MM-dd HH:mm"
        return f
    }

    /// What a `<Time>` shows.
    static func show(_ props: [String: Any]) -> String {
        guard let ms = (props["value"] as? NSNumber)?.doubleValue, ms.isFinite else { return "" }
        let date = Date(timeIntervalSince1970: ms / 1000)
        let show = props["show"] as? String ?? "datetime"
        let tz = zone(props["zone"] as? String)
        if props["style"] as? String == "iso" {
            let f = iso(show == "date" ? "date" : show == "time" ? "datetime" : "datetime", tz)
            let s = f.string(from: date)
            return show == "date" ? s : show == "time" ? String(s.suffix(5)) : s
        }
        let f = DateFormatter()
        f.timeZone = tz
        f.dateStyle = show == "time" ? .none : .medium
        f.timeStyle = show == "date" ? .none : .short
        return f.string(from: date)
    }

    /// A typed input's text as its value, or null when it is not one.
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

    /// A typed input's value as the text it shows.
    static func text(_ type: String, _ zone: String?, _ value: Any?) -> String {
        guard let ms = (value as? NSNumber)?.doubleValue, ms.isFinite else { return "" }
        if type == "number" { return ms == ms.rounded() && abs(ms) < 1e15 ? String(Int64(ms)) : String(ms) }
        return iso(type, self.zone(zone)).string(from: Date(timeIntervalSince1970: ms / 1000))
    }

    /// The system picker for a date, time or datetime input, set to the current value.
    static func picker(_ type: String, _ zone: String?, _ value: Any?) -> UIDatePicker {
        let p = UIDatePicker()
        p.datePickerMode = type == "date" ? .date : type == "time" ? .time : .dateAndTime
        p.preferredDatePickerStyle = .wheels
        p.timeZone = type == "time" ? TimeZone(identifier: "UTC")! : self.zone(zone)
        if let ms = (value as? NSNumber)?.doubleValue, ms.isFinite { p.date = Date(timeIntervalSince1970: ms / 1000) }
        return p
    }

    /// The picker's choice as a value: a time of day counts from midnight.
    static func picked(_ p: UIDatePicker) -> Double {
        let ms = (p.date.timeIntervalSince1970 * 1000).rounded()
        guard p.datePickerMode == .time else { return ms }
        let day = 86_400_000.0
        return ms - (ms / day).rounded(.down) * day
    }
}
