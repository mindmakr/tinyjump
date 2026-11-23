import AVFoundation
import CoreLocation
import Foundation
import MobileCoreServices
import UIKit
import UserNotifications
#if canImport(UniformTypeIdentifiers)
import UniformTypeIdentifiers
#endif

/// Opt-in iOS device capability plugins (pure Apple system frameworks, iOS 13+, zero external deps).
/// Copied into generated iOS apps by `tj build --ios` only when the app declares matching
/// device permissions (`files`, `notifications`, `location`, `camera`).
public enum DevicePlugins {
    private static let defaultMaxFileBytes = 10 * 1024 * 1024
    private static let hardMaxFileBytes = 10 * 1024 * 1024
    private static let maxBase64Chars = ((hardMaxFileBytes + 2) / 3) * 4 + 4
    private static var nextNotifId = 1

    private static weak var presenter: UIViewController?
    private static var registeredCaps: Set<String> = []
    private static var activePickDelegate: FilePickDelegate?
    private static var activeSaveDelegate: FileSaveDelegate?
    private static var activeCameraDelegate: CameraDelegate?
    private static var requestingCameraAuth = false
    private static var activeLocationDelegate: LocationDelegate?

    public static func register(presenter: UIViewController, capabilities: Set<String>) {
        unregister()
        self.presenter = presenter
        if capabilities.contains("files") {
            registeredCaps.insert("files")
            TinyJump.registerPlugin("files") { method, args, done in
                handleFiles(method: method, args: args, done: done)
            }
        }
        if capabilities.contains("notifications") {
            registeredCaps.insert("notifications")
            TinyJump.registerPlugin("notifications") { method, args, done in
                handleNotifications(method: method, args: args, done: done)
            }
        }
        if capabilities.contains("location") || capabilities.contains("location-background") {
            registeredCaps.insert("location")
            TinyJump.registerPlugin("location") { method, args, done in
                handleLocation(method: method, args: args, done: done)
            }
        }
        if capabilities.contains("camera") {
            registeredCaps.insert("camera")
            TinyJump.registerPlugin("camera") { method, args, done in
                handleCamera(method: method, args: args, done: done)
            }
        }
    }

    public static func unregister() {
        for cap in registeredCaps {
            TinyJump.registerPlugin(cap, nil)
        }
        registeredCaps.removeAll()
        activePickDelegate?.cancel(error: "plugin unregistered")
        activePickDelegate = nil
        activeSaveDelegate?.cancel(error: "plugin unregistered")
        activeSaveDelegate = nil
        activeCameraDelegate?.cancel(error: "plugin unregistered")
        activeCameraDelegate = nil
        requestingCameraAuth = false
        activeLocationDelegate?.cancel(error: "plugin unregistered")
        activeLocationDelegate = nil
        presenter = nil
    }

    private static func isModalActive() -> Bool {
        return activePickDelegate != nil || activeSaveDelegate != nil || activeCameraDelegate != nil || requestingCameraAuth
    }

    private static func clampInt(_ value: Any?, fallback: Int, min lo: Int, max hi: Int) -> Int {
        guard let num = value as? NSNumber else { return fallback }
        if CFGetTypeID(num) == CFBooleanGetTypeID() { return fallback }
        let d = num.doubleValue
        guard d.isFinite else { return fallback }
        let clamped = Swift.max(Double(lo), Swift.min(Double(hi), d))
        return Int(floor(clamped))
    }

    private static func clampDouble(_ value: Any?, fallback: Double, min lo: Double, max hi: Double) -> Double {
        guard let num = value as? NSNumber else { return fallback }
        if CFGetTypeID(num) == CFBooleanGetTypeID() { return fallback }
        let d = num.doubleValue
        guard d.isFinite else { return fallback }
        return Swift.max(lo, Swift.min(hi, d))
    }

    private static func sanitizeFilename(_ raw: String?) -> String? {
        guard let raw = raw else { return nil }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty || trimmed == "." || trimmed == ".." { return nil }
        let cleaned = trimmed
            .replacingOccurrences(of: "[\\\\/]+", with: "_", options: .regularExpression)
            .replacingOccurrences(of: "\\.\\.+", with: "_", options: .regularExpression)
            .replacingOccurrences(of: "[\\x00-\\x1f\\x7f]", with: "", options: .regularExpression)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if cleaned.isEmpty || cleaned == "." || cleaned == ".." || cleaned.allSatisfy({ $0 == "_" }) { return nil }
        return String(cleaned.prefix(180))
    }

    private static func topPresenter() -> UIViewController? {
        guard var vc = presenter else { return nil }
        while let next = vc.presentedViewController {
            vc = next
        }
        guard vc.viewIfLoaded?.window != nil, !vc.isBeingDismissed, !vc.isBeingPresented else { return nil }
        return vc
    }

    private static func configurePopover(_ controller: UIViewController, anchor: UIViewController) {
        if let pop = controller.popoverPresentationController {
            pop.sourceView = anchor.view
            pop.sourceRect = CGRect(x: anchor.view.bounds.midX, y: anchor.view.bounds.midY, width: 1, height: 1)
            pop.permittedArrowDirections = []
        }
    }

    private static func readBoundedFile(url: URL, maxBytes: Int) throws -> Data {
        let vals = try url.resourceValues(forKeys: [.isDirectoryKey, .fileSizeKey])
        if vals.isDirectory == true {
            throw NSError(domain: "TinyJump", code: 1, userInfo: [NSLocalizedDescriptionKey: "selected item is a directory, not a file"])
        }
        if let fileSize = vals.fileSize, fileSize > maxBytes {
            throw NSError(domain: "TinyJump", code: 2, userInfo: [NSLocalizedDescriptionKey: "file exceeds maximum size limit"])
        }
        if #available(iOS 13.4, *) {
            let handle = try FileHandle(forReadingFrom: url)
            defer { try? handle.close() }
            let raw = try handle.read(upToCount: maxBytes + 1) ?? Data()
            if raw.count > maxBytes {
                throw NSError(domain: "TinyJump", code: 2, userInfo: [NSLocalizedDescriptionKey: "file exceeds maximum size limit"])
            }
            return raw
        } else {
            guard let stream = InputStream(url: url) else {
                throw NSError(domain: "TinyJump", code: 3, userInfo: [NSLocalizedDescriptionKey: "cannot open selected file"])
            }
            stream.open()
            defer { stream.close() }
            var out = Data()
            let bufSize = 8192
            var buf = [UInt8](repeating: 0, count: bufSize)
            while true {
                let n = stream.read(&buf, maxLength: bufSize)
                if n < 0 {
                    throw stream.streamError ?? NSError(domain: "TinyJump", code: 4, userInfo: [NSLocalizedDescriptionKey: "failed to read file"])
                }
                if n == 0 { break }
                if out.count + n > maxBytes {
                    throw NSError(domain: "TinyJump", code: 2, userInfo: [NSLocalizedDescriptionKey: "file exceeds maximum size limit"])
                }
                out.append(buf, count: n)
            }
            return out
        }
    }

    #if canImport(UniformTypeIdentifiers)
    @available(iOS 14.0, *)
    private static func contentTypes(forAccept accept: String?) -> [UTType] {
        guard let accept = accept, !accept.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return [UTType.item]
        }
        var types: [UTType] = []
        for rawPart in accept.split(separator: ",") {
            let part = rawPart.trimmingCharacters(in: .whitespacesAndNewlines)
            if part.isEmpty { continue }
            if part == "*/*" { return [UTType.item] }
            if part.hasPrefix(".") {
                let ext = String(part.dropFirst())
                if let ut = UTType(filenameExtension: ext) { types.append(ut) }
            } else if part == "image/*" {
                types.append(UTType.image)
            } else if part == "audio/*" {
                types.append(UTType.audio)
            } else if part == "video/*" {
                types.append(UTType.movie)
            } else if part == "text/*" {
                types.append(UTType.text)
            } else if let ut = UTType(mimeType: part) {
                types.append(ut)
            }
        }
        return types.isEmpty ? [UTType.item] : types
    }
    #endif

    private static func legacyDocumentTypes(forAccept accept: String?) -> [String] {
        guard let accept = accept, !accept.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return [kUTTypeItem as String]
        }
        var types: [String] = []
        for rawPart in accept.split(separator: ",") {
            let part = rawPart.trimmingCharacters(in: .whitespacesAndNewlines)
            if part.isEmpty { continue }
            if part == "*/*" { return [kUTTypeItem as String] }
            if part == "image/*" {
                types.append(kUTTypeImage as String)
            } else if part == "audio/*" {
                types.append(kUTTypeAudio as String)
            } else if part == "video/*" {
                types.append(kUTTypeMovie as String)
            } else if part == "text/*" {
                types.append(kUTTypeText as String)
            } else if part.hasPrefix(".") {
                let ext = String(part.dropFirst()) as CFString
                if let uti = UTTypeCreatePreferredIdentifierForTag(kUTTagClassFilenameExtension, ext, nil)?.takeRetainedValue() {
                    types.append(uti as String)
                }
            } else if let uti = UTTypeCreatePreferredIdentifierForTag(kUTTagClassMIMEType, part as CFString, nil)?.takeRetainedValue() {
                types.append(uti as String)
            }
        }
        return types.isEmpty ? [kUTTypeItem as String] : types
    }

    // MARK: - files

    private static func handleFiles(method: String, args: [String: Any], done: @escaping TinyJump.Reply) {
        if method == "pick" {
            guard !isModalActive() else {
                done(nil, "a file or camera picker is already open")
                return
            }
            if let rawReadAs = args["readAs"], !(rawReadAs is String || rawReadAs is NSNull) {
                done(nil, "files.pick \"readAs\" must be \"text\" or \"base64\"")
                return
            }
            let readAs = (args["readAs"] as? String) ?? "text"
            guard readAs == "text" || readAs == "base64" else {
                done(nil, "files.pick \"readAs\" must be \"text\" or \"base64\"")
                return
            }
            let maxBytes = clampInt(args["maxBytes"], fallback: defaultMaxFileBytes, min: 1, max: hardMaxFileBytes)
            let multiple = (args["multiple"] as? Bool) ?? false
            let accept = args["accept"] as? String
            guard let vc = topPresenter() else {
                done(nil, "no active view controller")
                return
            }
            let picker: UIDocumentPickerViewController
            #if canImport(UniformTypeIdentifiers)
            if #available(iOS 14.0, *) {
                picker = UIDocumentPickerViewController(forOpeningContentTypes: contentTypes(forAccept: accept), asCopy: true)
            } else {
                picker = UIDocumentPickerViewController(documentTypes: legacyDocumentTypes(forAccept: accept), in: .import)
            }
            #else
            picker = UIDocumentPickerViewController(documentTypes: legacyDocumentTypes(forAccept: accept), in: .import)
            #endif
            picker.allowsMultipleSelection = multiple
            configurePopover(picker, anchor: vc)
            let delegate = FilePickDelegate(readAs: readAs, maxBytes: maxBytes) { ok, err in
                activePickDelegate = nil
                done(ok, err)
            }
            activePickDelegate = delegate
            picker.delegate = delegate
            picker.presentationController?.delegate = delegate
            vc.present(picker, animated: true)
            return
        }

        if method == "save" {
            guard !isModalActive() else {
                done(nil, "a file or camera picker is already open")
                return
            }
            guard let name = sanitizeFilename(args["name"] as? String) else {
                done(nil, "files.save requires a valid filename")
                return
            }
            let textVal = args["text"] as? String
            let b64Val = args["base64"] as? String
            if textVal == nil && b64Val == nil {
                done(nil, "files.save requires either \"text\" or \"base64\"")
                return
            }
            if textVal != nil && b64Val != nil {
                done(nil, "files.save accepts either \"text\" or \"base64\", not both")
                return
            }
            let data: Data
            if let b64 = b64Val {
                let compact = b64.components(separatedBy: .whitespacesAndNewlines).joined()
                if compact.count > maxBase64Chars {
                    done(nil, "file exceeds maximum size limit")
                    return
                }
                guard let decoded = Data(base64Encoded: compact, options: []) else {
                    done(nil, "invalid base64 payload")
                    return
                }
                data = decoded
            } else {
                data = Data((textVal ?? "").utf8)
            }
            if data.count > hardMaxFileBytes {
                done(nil, "file exceeds maximum size limit")
                return
            }
            guard let vc = topPresenter() else {
                done(nil, "no active view controller")
                return
            }
            let tempDir = FileManager.default.temporaryDirectory.appendingPathComponent("tj-save-\(UUID().uuidString)", isDirectory: true)
            let tempFile = tempDir.appendingPathComponent(name)
            do {
                try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
                try data.write(to: tempFile, options: .atomic)
            } catch {
                try? FileManager.default.removeItem(at: tempDir)
                done(nil, "failed to prepare file for saving: \(error.localizedDescription)")
                return
            }
            let picker: UIDocumentPickerViewController
            #if canImport(UniformTypeIdentifiers)
            if #available(iOS 14.0, *) {
                picker = UIDocumentPickerViewController(forExporting: [tempFile], asCopy: true)
            } else {
                picker = UIDocumentPickerViewController(url: tempFile, in: .exportToService)
            }
            #else
            picker = UIDocumentPickerViewController(url: tempFile, in: .exportToService)
            #endif
            configurePopover(picker, anchor: vc)
            let delegate = FileSaveDelegate(name: name, cleanupDir: tempDir) { ok, err in
                activeSaveDelegate = nil
                done(ok, err)
            }
            activeSaveDelegate = delegate
            picker.delegate = delegate
            picker.presentationController?.delegate = delegate
            vc.present(picker, animated: true)
            return
        }

        done(nil, "unknown files method: \(method)")
    }

    private final class FilePickDelegate: NSObject, UIDocumentPickerDelegate, UIAdaptivePresentationControllerDelegate {
        private let readAs: String
        private let maxBytes: Int
        private var finish: TinyJump.Reply?

        init(readAs: String, maxBytes: Int, finish: @escaping TinyJump.Reply) {
            self.readAs = readAs
            self.maxBytes = maxBytes
            self.finish = finish
        }

        func cancel(error: String) {
            settle(ok: nil, error: error)
        }

        private func settle(ok: Any?, error: String?) {
            guard let f = finish else { return }
            finish = nil
            f(ok, error)
        }

        func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
            settle(ok: ["cancelled": true, "files": []], error: nil)
        }

        func presentationControllerDidDismiss(_ presentationController: UIPresentationController) {
            settle(ok: ["cancelled": true, "files": []], error: nil)
        }

        func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
            let readAs = self.readAs
            let maxBytes = self.maxBytes
            DispatchQueue.global(qos: .userInitiated).async { [weak self] in
                var entries: [[String: Any]] = []
                var remaining = maxBytes
                for url in urls {
                    let accessed = url.startAccessingSecurityScopedResource()
                    defer { if accessed { url.stopAccessingSecurityScopedResource() } }
                    do {
                        let raw = try DevicePlugins.readBoundedFile(url: url, maxBytes: remaining)
                        remaining -= raw.count
                        var item: [String: Any] = [
                            "name": url.lastPathComponent,
                            "mime": mimeTypeForUrl(url),
                            "size": raw.count,
                        ]
                        if readAs == "base64" {
                            item["base64"] = raw.base64EncodedString()
                        } else {
                            item["text"] = String(decoding: raw, as: UTF8.self)
                        }
                        entries.append(item)
                    } catch {
                        DispatchQueue.main.async { self?.settle(ok: nil, error: error.localizedDescription) }
                        return
                    }
                }
                DispatchQueue.main.async {
                    self?.settle(ok: ["cancelled": entries.isEmpty, "files": entries], error: nil)
                }
            }
        }
    }

    private static func mimeTypeForUrl(_ url: URL) -> String {
        #if canImport(UniformTypeIdentifiers)
        if #available(iOS 14.0, *) {
            if let ut = UTType(filenameExtension: url.pathExtension), let mime = ut.preferredMIMEType {
                return mime
            }
        }
        #endif
        return "application/octet-stream"
    }

    private final class FileSaveDelegate: NSObject, UIDocumentPickerDelegate, UIAdaptivePresentationControllerDelegate {
        private let name: String
        private let cleanupDir: URL
        private var finish: TinyJump.Reply?

        init(name: String, cleanupDir: URL, finish: @escaping TinyJump.Reply) {
            self.name = name
            self.cleanupDir = cleanupDir
            self.finish = finish
        }

        func cancel(error: String) {
            settle(ok: nil, error: error)
        }

        private func settle(ok: Any?, error: String?) {
            try? FileManager.default.removeItem(at: cleanupDir)
            guard let f = finish else { return }
            finish = nil
            f(ok, error)
        }

        func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
            settle(ok: ["saved": false, "cancelled": true, "name": name], error: nil)
        }

        func presentationControllerDidDismiss(_ presentationController: UIPresentationController) {
            settle(ok: ["saved": false, "cancelled": true, "name": name], error: nil)
        }

        func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
            settle(ok: ["saved": true, "cancelled": false, "name": name], error: nil)
        }
    }

    // MARK: - notifications

    private static func handleNotifications(method: String, args: [String: Any], done: @escaping TinyJump.Reply) {
        let center = UNUserNotificationCenter.current()
        if method == "requestPermission" {
            center.requestAuthorization(options: [.alert, .sound, .badge]) { granted, error in
                if let error = error {
                    done(nil, error.localizedDescription)
                } else {
                    done(["granted": granted, "status": granted ? "granted" : "denied"], nil)
                }
            }
            return
        }

        if method == "notify" {
            let title = ((args["title"] as? String) ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            if title.isEmpty {
                done(nil, "notifications.notify requires a non-empty \"title\"")
                return
            }
            if title.count > 256 {
                done(nil, "notifications.notify \"title\" is too long")
                return
            }
            let body = String(((args["body"] as? String) ?? "").prefix(2048))
            let rawId = ((args["id"] as? String) ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            let id: String
            if rawId.isEmpty {
                id = "tj-\(nextNotifId)"
                nextNotifId += 1
            } else {
                id = String(rawId.prefix(128))
            }
            center.getNotificationSettings { settings in
                let ok = settings.authorizationStatus == .authorized || settings.authorizationStatus == .provisional
                guard ok else {
                    done(nil, "notification permission not granted")
                    return
                }
                let content = UNMutableNotificationContent()
                content.title = title
                content.body = body
                content.sound = .default
                let req = UNNotificationRequest(identifier: id, content: content, trigger: nil)
                center.add(req) { error in
                    if let error = error {
                        done(nil, error.localizedDescription)
                    } else {
                        done(["delivered": true, "id": id], nil)
                    }
                }
            }
            return
        }

        done(nil, "unknown notifications method: \(method)")
    }

    // MARK: - location

    private static func handleLocation(method: String, args: [String: Any], done: @escaping TinyJump.Reply) {
        guard method == "currentPosition" else {
            done(nil, "unknown location method: \(method)")
            return
        }
        guard activeLocationDelegate == nil else {
            done(nil, "a location request is already in progress")
            return
        }
        let highAccuracy = (args["highAccuracy"] as? Bool) ?? false
        let timeoutMs = clampInt(args["timeoutMs"], fallback: 15000, min: 1000, max: 60000)
        let delegate = LocationDelegate(highAccuracy: highAccuracy, timeoutMs: timeoutMs) { ok, err in
            activeLocationDelegate = nil
            done(ok, err)
        }
        activeLocationDelegate = delegate
        delegate.start()
    }

    private final class LocationDelegate: NSObject, CLLocationManagerDelegate {
        private let manager = CLLocationManager()
        private let timeoutMs: Int
        private var timer: Timer?
        private var locationRequested = false
        private var finish: TinyJump.Reply?

        init(highAccuracy: Bool, timeoutMs: Int, finish: @escaping TinyJump.Reply) {
            self.timeoutMs = timeoutMs
            self.finish = finish
            super.init()
            manager.desiredAccuracy = highAccuracy ? kCLLocationAccuracyBest : kCLLocationAccuracyHundredMeters
        }

        func start() {
            // Assign delegate AFTER activeLocationDelegate is stored, because iOS 14+ invokes
            // locationManagerDidChangeAuthorization synchronously on assignment.
            manager.delegate = self
            guard finish != nil else { return }
            let status: CLAuthorizationStatus
            if #available(iOS 14.0, *) {
                status = manager.authorizationStatus
            } else {
                status = CLLocationManager.authorizationStatus()
            }
            switch status {
            case .authorizedWhenInUse, .authorizedAlways:
                beginRequest()
            case .notDetermined:
                scheduleTimeout()
                manager.requestWhenInUseAuthorization()
            default:
                settle(ok: nil, error: "location permission denied")
            }
        }

        func cancel(error: String) {
            settle(ok: nil, error: error)
        }

        private func scheduleTimeout() {
            timer?.invalidate()
            timer = Timer.scheduledTimer(withTimeInterval: Double(timeoutMs) / 1000.0, repeats: false) { [weak self] _ in
                self?.settle(ok: nil, error: "location request timed out")
            }
        }

        private func beginRequest() {
            guard finish != nil && !locationRequested else { return }
            locationRequested = true
            scheduleTimeout()
            manager.requestLocation()
        }

        private func settle(ok: Any?, error: String?) {
            timer?.invalidate()
            timer = nil
            manager.stopUpdatingLocation()
            manager.delegate = nil
            guard let f = finish else { return }
            finish = nil
            f(ok, error)
        }

        func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
            if #available(iOS 14.0, *) {
                handleAuth(manager.authorizationStatus)
            }
        }

        func locationManager(_ manager: CLLocationManager, didChangeAuthorization status: CLAuthorizationStatus) {
            handleAuth(status)
        }

        private func handleAuth(_ status: CLAuthorizationStatus) {
            guard finish != nil else { return }
            switch status {
            case .authorizedWhenInUse, .authorizedAlways:
                beginRequest()
            case .notDetermined:
                break
            default:
                settle(ok: nil, error: "location permission denied")
            }
        }

        func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
            guard let loc = locations.last else { return }
            let lat = loc.coordinate.latitude
            let lng = loc.coordinate.longitude
            guard lat.isFinite && lng.isFinite else {
                settle(ok: nil, error: "location returned non-finite coordinates")
                return
            }
            let acc = loc.horizontalAccuracy.isFinite && loc.horizontalAccuracy >= 0 ? loc.horizontalAccuracy : 0.0
            let alt: Any = (loc.verticalAccuracy >= 0 && loc.altitude.isFinite) ? loc.altitude : NSNull()
            let ts = loc.timestamp.timeIntervalSince1970 * 1000.0
            settle(ok: [
                "lat": lat,
                "lng": lng,
                "accuracy": acc,
                "altitude": alt,
                "timestamp": ts.isFinite ? ts : Date().timeIntervalSince1970 * 1000.0,
            ], error: nil)
        }

        func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) {
            settle(ok: nil, error: error.localizedDescription)
        }
    }

    // MARK: - camera

    private static func handleCamera(method: String, args: [String: Any], done: @escaping TinyJump.Reply) {
        guard method == "capture" else {
            done(nil, "unknown camera method: \(method)")
            return
        }
        guard !isModalActive() else {
            done(nil, "a file or camera picker is already open")
            return
        }
        if let rawFacing = args["facing"], !(rawFacing is String || rawFacing is NSNull) {
            done(nil, "camera.capture \"facing\" must be \"back\" or \"front\"")
            return
        }
        let facing = (args["facing"] as? String) ?? "back"
        guard facing == "back" || facing == "front" else {
            done(nil, "camera.capture \"facing\" must be \"back\" or \"front\"")
            return
        }
        let maxDim = clampInt(args["maxDimension"], fallback: 1280, min: 64, max: 4096)
        let quality = clampDouble(args["quality"], fallback: 0.85, min: 0.1, max: 1.0)
        guard UIImagePickerController.isSourceTypeAvailable(.camera) else {
            done(nil, "camera is not available on this device")
            return
        }
        let present = {
            guard let vc = topPresenter() else {
                done(nil, "no active view controller")
                return
            }
            let picker = UIImagePickerController()
            picker.sourceType = .camera
            let desired: UIImagePickerController.CameraDevice = (facing == "front") ? .front : .rear
            if UIImagePickerController.isCameraDeviceAvailable(desired) {
                picker.cameraDevice = desired
            }
            configurePopover(picker, anchor: vc)
            let delegate = CameraDelegate(maxDimension: maxDim, quality: quality) { ok, err in
                activeCameraDelegate = nil
                done(ok, err)
            }
            activeCameraDelegate = delegate
            picker.delegate = delegate
            picker.presentationController?.delegate = delegate
            vc.present(picker, animated: true)
        }
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            present()
        case .notDetermined:
            requestingCameraAuth = true
            AVCaptureDevice.requestAccess(for: .video) { granted in
                DispatchQueue.main.async {
                    requestingCameraAuth = false
                    guard presenter != nil else {
                        done(nil, "plugin unregistered")
                        return
                    }
                    if granted { present() } else { done(nil, "camera permission denied") }
                }
            }
        default:
            done(nil, "camera permission denied")
        }
    }

    private final class CameraDelegate: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate, UIAdaptivePresentationControllerDelegate {
        private let maxDimension: Int
        private let quality: Double
        private var finish: TinyJump.Reply?

        init(maxDimension: Int, quality: Double, finish: @escaping TinyJump.Reply) {
            self.maxDimension = maxDimension
            self.quality = quality
            self.finish = finish
        }

        func cancel(error: String) {
            settle(ok: nil, error: error)
        }

        private func settle(ok: Any?, error: String?) {
            guard let f = finish else { return }
            finish = nil
            f(ok, error)
        }

        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
            picker.dismiss(animated: true) { [weak self] in
                self?.settle(ok: ["cancelled": true, "mime": "", "width": 0, "height": 0, "base64": ""], error: nil)
            }
        }

        func presentationControllerDidDismiss(_ presentationController: UIPresentationController) {
            settle(ok: ["cancelled": true, "mime": "", "width": 0, "height": 0, "base64": ""], error: nil)
        }

        func imagePickerController(_ picker: UIImagePickerController, didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            guard let image = info[.originalImage] as? UIImage else {
                picker.dismiss(animated: true) { [weak self] in
                    self?.settle(ok: nil, error: "camera did not return an image")
                }
                return
            }
            let maxDim = CGFloat(maxDimension)
            let quality = CGFloat(self.quality)
            picker.dismiss(animated: true) { [weak self] in
                DispatchQueue.global(qos: .userInitiated).async {
                    let origW = image.size.width * image.scale
                    let origH = image.size.height * image.scale
                    guard origW > 0 && origH > 0 else {
                        DispatchQueue.main.async { self?.settle(ok: nil, error: "captured image has invalid dimensions") }
                        return
                    }
                    var targetW = origW
                    var targetH = origH
                    if targetW > maxDim || targetH > maxDim {
                        let scale = min(maxDim / targetW, maxDim / targetH)
                        targetW = max(1, (targetW * scale).rounded())
                        targetH = max(1, (targetH * scale).rounded())
                    }
                    let format = UIGraphicsImageRendererFormat()
                    format.scale = 1.0
                    let renderer = UIGraphicsImageRenderer(size: CGSize(width: targetW, height: targetH), format: format)
                    let rendered = renderer.image { _ in
                        image.draw(in: CGRect(x: 0, y: 0, width: targetW, height: targetH))
                    }
                    guard let jpeg = rendered.jpegData(compressionQuality: quality) else {
                        DispatchQueue.main.async { self?.settle(ok: nil, error: "failed to encode captured image") }
                        return
                    }
                    DispatchQueue.main.async {
                        self?.settle(ok: [
                            "cancelled": false,
                            "mime": "image/jpeg",
                            "width": Int(targetW),
                            "height": Int(targetH),
                            "base64": jpeg.base64EncodedString(),
                        ], error: nil)
                    }
                }
            }
        }
    }
}
