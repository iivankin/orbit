import AppKit
import ApplicationServices
import Foundation
import ScreenCaptureKit

enum DriverError: Error, CustomStringConvertible {
    case usage(String)
    case missingFlag(String)
    case invalidFlag(String, String)
    case accessibilityPermission
    case appNotRunning(String)
    case pointHitTest(Double, Double)
    case helper(String)

    var description: String {
        switch self {
        case .usage(let message):
            return message
        case .missingFlag(let flag):
            return "missing required flag \(flag)"
        case .invalidFlag(let flag, let value):
            return "invalid value `\(value)` for \(flag)"
        case .accessibilityPermission:
            return "macOS UI automation requires Accessibility access for Orbi or the calling terminal. Enable it in System Settings > Privacy & Security > Accessibility and try again."
        case .appNotRunning(let bundleID):
            return "application `\(bundleID)` is not running"
        case .pointHitTest(let x, let y):
            return "could not resolve an accessibility element at point (\(x), \(y))"
        case .helper(let message):
            return message
        }
    }
}

struct ElementSnapshot {
    let dictionary: [String: Any]
    let children: [AXUIElement]
}

struct WindowCaptureInfo {
    let windowNumber: Int
    let frame: CGRect

    var dictionary: [String: Any] {
        [
            "windowNumber": windowNumber,
            "frame": [
                "x": frame.origin.x,
                "y": frame.origin.y,
                "width": frame.size.width,
                "height": frame.size.height,
            ],
        ]
    }
}

struct DoctorStatus {
    let accessibilityTrusted: Bool
    let screenCaptureAccess: Bool

    var dictionary: [String: Any] {
        [
            "accessibilityTrusted": accessibilityTrusted,
            "screenCaptureAccess": screenCaptureAccess,
        ]
    }
}

struct FrontmostApplicationInfo {
    let pid: pid_t
    let bundleID: String?

    var dictionary: [String: Any] {
        [
            "pid": Int(pid),
            "bundleId": bundleID ?? NSNull(),
        ]
    }
}

struct LaunchedApplicationInfo {
    let pid: pid_t
    let bundleID: String?

    var dictionary: [String: Any] {
        [
            "pid": Int(pid),
            "bundleId": bundleID ?? NSNull(),
        ]
    }
}

struct BridgeConfig {
    let directory: String
    let notificationName: String
}

struct BridgeResponse: Decodable {
    let ok: Bool
    let error: String?
}

enum ApplicationTarget {
    case bundleID(String)
    case pid(pid_t)

    var description: String {
        switch self {
        case .bundleID(let bundleID):
            return bundleID
        case .pid(let pid):
            return "pid \(pid)"
        }
    }
}

private let childAttributes = [
    kAXChildrenAttribute as String,
    kAXVisibleChildrenAttribute as String,
    kAXWindowsAttribute as String,
    "AXSheets",
    kAXContentsAttribute as String,
    "AXToolbar",
    kAXTabsAttribute as String,
    kAXRowsAttribute as String,
    kAXColumnsAttribute as String,
    kAXFocusedWindowAttribute as String,
]

func fail(_ error: Error) -> Never {
    fputs("\(error)\n", stderr)
    exit(1)
}

func requireFlag(_ name: String, in arguments: [String]) throws -> String {
    guard let index = arguments.firstIndex(of: name), index + 1 < arguments.count else {
        throw DriverError.missingFlag(name)
    }
    return arguments[index + 1]
}

func requireDoubleFlag(_ name: String, in arguments: [String]) throws -> Double {
    let raw = try requireFlag(name, in: arguments)
    guard let value = Double(raw) else {
        throw DriverError.invalidFlag(name, raw)
    }
    return value
}

func requireIntFlag(_ name: String, in arguments: [String]) throws -> Int {
    let raw = try requireFlag(name, in: arguments)
    guard let value = Int(raw) else {
        throw DriverError.invalidFlag(name, raw)
    }
    return value
}

func optionalIntFlag(_ name: String, in arguments: [String]) throws -> Int? {
    guard arguments.contains(name) else {
        return nil
    }
    return try requireIntFlag(name, in: arguments)
}

func optionalFlag(_ name: String, in arguments: [String]) throws -> String? {
    guard arguments.contains(name) else {
        return nil
    }
    return try requireFlag(name, in: arguments)
}

func optionalBridgeConfig(in arguments: [String]) throws -> BridgeConfig? {
    guard let directory = try optionalFlag("--bridge-dir", in: arguments),
          let notificationName = try optionalFlag("--bridge-name", in: arguments)
    else {
        return nil
    }
    return BridgeConfig(directory: directory, notificationName: notificationName)
}

func repeatedFlags(_ name: String, in arguments: [String]) throws -> [String] {
    var values = [String]()
    var index = 0
    while index < arguments.count {
        if arguments[index] == name {
            guard index + 1 < arguments.count else {
                throw DriverError.missingFlag(name)
            }
            values.append(arguments[index + 1])
            index += 2
            continue
        }
        index += 1
    }
    guard !values.isEmpty else {
        throw DriverError.missingFlag(name)
    }
    return values
}

func optionalRepeatedFlags(_ name: String, in arguments: [String]) throws -> [String] {
    var values = [String]()
    var index = 0
    while index < arguments.count {
        if arguments[index] == name {
            guard index + 1 < arguments.count else {
                throw DriverError.missingFlag(name)
            }
            values.append(arguments[index + 1])
            index += 2
            continue
        }
        index += 1
    }
    return values
}

func requireApplicationTarget(in arguments: [String]) throws -> ApplicationTarget {
    if let pidValue = try optionalIntFlag("--pid", in: arguments) {
        return .pid(pid_t(pidValue))
    }
    if let bundleID = try optionalFlag("--bundle-id", in: arguments) {
        return .bundleID(bundleID)
    }
    throw DriverError.usage("expected either --pid or --bundle-id")
}

func optionalApplicationTarget(in arguments: [String]) throws -> ApplicationTarget? {
    if let pidValue = try optionalIntFlag("--pid", in: arguments) {
        return .pid(pid_t(pidValue))
    }
    if let bundleID = try optionalFlag("--bundle-id", in: arguments) {
        return .bundleID(bundleID)
    }
    return nil
}

func optionalTargetPID(in arguments: [String]) throws -> pid_t? {
    guard let target = try optionalApplicationTarget(in: arguments) else {
        return nil
    }
    return try targetProcessIdentifier(target: target)
}

func ensureAccessibilityPermission() throws {
    guard AXIsProcessTrusted() else {
        throw DriverError.accessibilityPermission
    }
}

func processExists(_ pid: pid_t) -> Bool {
    kill(pid, 0) == 0 || errno == EPERM
}

func targetProcessIdentifier(target: ApplicationTarget) throws -> pid_t {
    switch target {
    case .bundleID:
        return try runningApplication(target: target).processIdentifier
    case .pid(let pid):
        guard processExists(pid) else {
            throw DriverError.appNotRunning("pid \(pid)")
        }
        return pid
    }
}

func runningApplication(target: ApplicationTarget) throws -> NSRunningApplication {
    switch target {
    case .bundleID(let bundleID):
        let applications = NSRunningApplication.runningApplications(withBundleIdentifier: bundleID)
            .filter { !$0.isTerminated }
        guard let application = applications.first else {
            throw DriverError.appNotRunning(bundleID)
        }
        return application
    case .pid(let pid):
        guard let application = NSRunningApplication(processIdentifier: pid), !application.isTerminated else {
            throw DriverError.appNotRunning("pid \(pid)")
        }
        return application
    }
}

func applicationElement(target: ApplicationTarget) throws -> (pid_t, AXUIElement) {
    let pid = try targetProcessIdentifier(target: target)
    return (pid, AXUIElementCreateApplication(pid))
}

func frontmostApplicationInfo() -> FrontmostApplicationInfo? {
    guard let application = NSWorkspace.shared.frontmostApplication else {
        return nil
    }
    return FrontmostApplicationInfo(
        pid: application.processIdentifier,
        bundleID: application.bundleIdentifier
    )
}

func environmentVariables(from entries: [String]) throws -> [String: String] {
    var environment = [String: String]()
    for entry in entries {
        guard let separator = entry.firstIndex(of: "=") else {
            throw DriverError.helper("launch environment entry `\(entry)` must be KEY=VALUE")
        }
        let key = String(entry[..<separator])
        guard !key.isEmpty else {
            throw DriverError.helper("launch environment entry `\(entry)` is missing a key")
        }
        let value = String(entry[entry.index(after: separator)...])
        environment[key] = value
    }
    return environment
}

func launchApplication(
    appPath: String,
    arguments: [String],
    environmentEntries: [String]
) throws -> LaunchedApplicationInfo {
    let configuration = NSWorkspace.OpenConfiguration()
    configuration.activates = false
    configuration.createsNewApplicationInstance = true
    configuration.allowsRunningApplicationSubstitution = false
    configuration.arguments = arguments
    configuration.environment = try environmentVariables(from: environmentEntries)

    let launchSemaphore = DispatchSemaphore(value: 0)
    var launchedApplication: NSRunningApplication?
    var launchError: Error?
    NSWorkspace.shared.openApplication(at: URL(fileURLWithPath: appPath), configuration: configuration) {
        application,
        error in
        launchedApplication = application
        launchError = error
        launchSemaphore.signal()
    }
    launchSemaphore.wait()

    if let launchError {
        throw launchError
    }
    guard let launchedApplication else {
        throw DriverError.helper("Launch Services did not return a running application")
    }
    return LaunchedApplicationInfo(
        pid: launchedApplication.processIdentifier,
        bundleID: launchedApplication.bundleIdentifier
    )
}

func reopenApplication(target: ApplicationTarget) throws {
    let targetDescriptor: NSAppleEventDescriptor
    switch target {
    case .bundleID(let bundleID):
        targetDescriptor = NSAppleEventDescriptor(bundleIdentifier: bundleID)
    case .pid(let pid):
        targetDescriptor = NSAppleEventDescriptor(processIdentifier: pid)
    }

    let event = NSAppleEventDescriptor(
        eventClass: AEEventClass(kCoreEventClass),
        eventID: AEEventID(kAEReopenApplication),
        targetDescriptor: targetDescriptor,
        returnID: AEReturnID(kAutoGenerateReturnID),
        transactionID: AETransactionID(kAnyTransactionID)
    )
    _ = try event.sendEvent(options: [], timeout: 5)
}

func frame(for element: AXUIElement) -> CGRect? {
    guard let dictionary = frameDictionary(for: element) else {
        return nil
    }
    return CGRect(
        x: dictionary["x"] ?? 0,
        y: dictionary["y"] ?? 0,
        width: dictionary["width"] ?? 0,
        height: dictionary["height"] ?? 0
    )
}

func focusedWindowFrame(target: ApplicationTarget) -> CGRect? {
    guard let (_, applicationElementRef) = try? applicationElement(target: target),
          let focusedWindow = attributeValue(
            applicationElementRef,
            attribute: kAXFocusedWindowAttribute as String
          ),
          CFGetTypeID(focusedWindow) == AXUIElementGetTypeID()
    else {
        return nil
    }
    return frame(for: focusedWindow as! AXUIElement)
}

func intersectionArea(_ lhs: CGRect, _ rhs: CGRect) -> CGFloat {
    let intersection = lhs.intersection(rhs)
    guard !intersection.isNull else {
        return 0
    }
    return intersection.width * intersection.height
}

func windowCaptureInfo(target: ApplicationTarget) throws -> WindowCaptureInfo {
    let pid = Int(try targetProcessIdentifier(target: target))
    guard let windows = CGWindowListCopyWindowInfo(
        [.optionOnScreenOnly, .excludeDesktopElements],
        kCGNullWindowID
    ) as? [[String: Any]] else {
        throw DriverError.helper("failed to enumerate macOS windows")
    }

    let candidateWindows = windows.compactMap { window -> WindowCaptureInfo? in
        guard let ownerPID = window[kCGWindowOwnerPID as String] as? Int,
              ownerPID == pid,
              let windowNumber = window[kCGWindowNumber as String] as? Int,
              let layer = window[kCGWindowLayer as String] as? Int,
              layer == 0,
              let bounds = window[kCGWindowBounds as String] as? NSDictionary,
              let frame = CGRect(dictionaryRepresentation: bounds),
              frame.width > 1,
              frame.height > 1
        else {
            return nil
        }

        if let alpha = window[kCGWindowAlpha as String] as? Double, alpha <= 0 {
            return nil
        }

        return WindowCaptureInfo(windowNumber: windowNumber, frame: frame)
    }

    if let focusedFrame = focusedWindowFrame(target: target),
       let focusedWindow = candidateWindows.max(by: { lhs, rhs in
           intersectionArea(lhs.frame, focusedFrame) < intersectionArea(rhs.frame, focusedFrame)
       }),
       intersectionArea(focusedWindow.frame, focusedFrame) > 0
    {
        return focusedWindow
    }

    let bestWindow = candidateWindows.max { lhs, rhs in
        (lhs.frame.width * lhs.frame.height) < (rhs.frame.width * rhs.frame.height)
    }

    guard let bestWindow else {
        throw DriverError.helper("could not find a visible macOS window for `\(target.description)`")
    }
    return bestWindow
}

func attributeValue(_ element: AXUIElement, attribute: String) -> CFTypeRef? {
    var value: CFTypeRef?
    let error = AXUIElementCopyAttributeValue(element, attribute as CFString, &value)
    guard error == .success else {
        return nil
    }
    return value
}

func stringAttribute(_ element: AXUIElement, attribute: String) -> String? {
    guard let value = attributeValue(element, attribute: attribute) else {
        return nil
    }
    return stringValue(from: value)
}

func stringValue(from value: CFTypeRef) -> String? {
    if let string = value as? String {
        return string
    }
    if let number = value as? NSNumber {
        return number.stringValue
    }
    if let boolValue = value as? Bool {
        return boolValue ? "true" : "false"
    }
    return nil
}

func frameDictionary(for element: AXUIElement) -> [String: Double]? {
    guard let positionValue = attributeValue(element, attribute: kAXPositionAttribute as String),
          let sizeValue = attributeValue(element, attribute: kAXSizeAttribute as String)
    else {
        return nil
    }

    var point = CGPoint.zero
    var size = CGSize.zero
    guard AXValueGetType(positionValue as! AXValue) == .cgPoint,
          AXValueGetValue(positionValue as! AXValue, .cgPoint, &point),
          AXValueGetType(sizeValue as! AXValue) == .cgSize,
          AXValueGetValue(sizeValue as! AXValue, .cgSize, &size)
    else {
        return nil
    }

    let components = [point.x, point.y, size.width, size.height]
    guard components.allSatisfy(\.isFinite) else {
        return nil
    }

    return [
        "x": Double(point.x),
        "y": Double(point.y),
        "width": Double(size.width),
        "height": Double(size.height),
    ]
}

func childElements(of element: AXUIElement) -> [AXUIElement] {
    var seen = Set<CFHashCode>()
    var results = [AXUIElement]()

    func append(_ candidate: AXUIElement) {
        let hash = CFHash(candidate)
        guard seen.insert(hash).inserted else {
            return
        }
        results.append(candidate)
    }

    for attribute in childAttributes {
        guard let value = attributeValue(element, attribute: attribute) else {
            continue
        }
        let typeID = CFGetTypeID(value)
        if typeID == AXUIElementGetTypeID() {
            append(value as! AXUIElement)
            continue
        }
        if typeID == CFArrayGetTypeID(), let array = value as? [Any] {
            for entry in array {
                if CFGetTypeID(entry as CFTypeRef) == AXUIElementGetTypeID() {
                    append(entry as! AXUIElement)
                }
            }
        }
    }

    return results
}

func serialize(_ element: AXUIElement) -> ElementSnapshot {
    let role = stringAttribute(element, attribute: kAXRoleAttribute as String)
    let subrole = stringAttribute(element, attribute: kAXSubroleAttribute as String)
    let title = stringAttribute(element, attribute: kAXTitleAttribute as String)
    let description = stringAttribute(element, attribute: kAXDescriptionAttribute as String)
    let identifier = stringAttribute(element, attribute: kAXIdentifierAttribute as String)
    let value = attributeValue(element, attribute: kAXValueAttribute as String).flatMap(stringValue)
    let frame = frameDictionary(for: element)

    var dictionary = [String: Any]()
    if let role {
        dictionary["AXRole"] = role
    }
    if let subrole {
        dictionary["AXSubrole"] = subrole
    }
    if let title, !title.isEmpty {
        dictionary["AXLabel"] = title
    } else if let description, !description.isEmpty {
        dictionary["AXLabel"] = description
    } else if let value, !value.isEmpty {
        dictionary["AXLabel"] = value
    }
    if let identifier, !identifier.isEmpty {
        dictionary["AXIdentifier"] = identifier
    }
    if let value, !value.isEmpty {
        dictionary["AXValue"] = value
    }
    if let frame {
        dictionary["frame"] = frame
    }

    return ElementSnapshot(dictionary: dictionary, children: childElements(of: element))
}

func collectSnapshots(from root: AXUIElement) -> [[String: Any]] {
    var visited = Set<CFHashCode>()
    var results = [[String: Any]]()

    func visit(_ element: AXUIElement) {
        let hash = CFHash(element)
        guard visited.insert(hash).inserted else {
            return
        }

        let snapshot = serialize(element)
        results.append(snapshot.dictionary)
        for child in snapshot.children {
            visit(child)
        }
    }

    visit(root)
    return results
}

struct SelectorMatch {
    let element: AXUIElement
    let score: Int
    let area: Double
}

func selectorScore(_ candidate: String?, needle: String) -> Int {
    guard let candidate = candidate?.trimmingCharacters(in: .whitespacesAndNewlines),
          !candidate.isEmpty
    else {
        return 0
    }

    if candidate == needle {
        return 4
    }
    if candidate.localizedCaseInsensitiveContains(needle) {
        return 2
    }
    return 0
}

func selectorMatch(
    for element: AXUIElement,
    text: String?,
    identifier: String?
) -> SelectorMatch? {
    let idScore = identifier.map {
        selectorScore(stringAttribute(element, attribute: kAXIdentifierAttribute as String), needle: $0)
    } ?? 1
    if identifier != nil && idScore == 0 {
        return nil
    }

    let textCandidates = [
        stringAttribute(element, attribute: kAXTitleAttribute as String),
        stringAttribute(element, attribute: kAXDescriptionAttribute as String),
        attributeValue(element, attribute: kAXValueAttribute as String).flatMap(stringValue),
    ]
    let textScore = text.map { needle in
        textCandidates.map { selectorScore($0, needle: needle) }.max() ?? 0
    } ?? 1
    if text != nil && textScore == 0 {
        return nil
    }

    let frame = frame(for: element)
    let area = frame.map { max(1, Double($0.width * $0.height)) } ?? Double.greatestFiniteMagnitude
    return SelectorMatch(
        element: element,
        score: idScore + textScore,
        area: area
    )
}

func bestMatchingElement(
    root: AXUIElement,
    text: String?,
    identifier: String?
) -> AXUIElement? {
    var visited = Set<CFHashCode>()
    var bestMatch: SelectorMatch?

    func visit(_ element: AXUIElement) {
        let hash = CFHash(element)
        guard visited.insert(hash).inserted else {
            return
        }

        if let candidate = selectorMatch(for: element, text: text, identifier: identifier) {
            if let current = bestMatch {
                if candidate.score > current.score
                    || (candidate.score == current.score && candidate.area < current.area)
                {
                    bestMatch = candidate
                }
            } else {
                bestMatch = candidate
            }
        }

        for child in childElements(of: element) {
            visit(child)
        }
    }

    visit(root)
    return bestMatch?.element
}

func pressElementOrAncestor(
    _ element: AXUIElement,
    targetPID: pid_t? = nil
) throws {
    var candidate: AXUIElement? = element
    for _ in 0..<6 {
        guard let current = candidate else {
            break
        }
        let role = stringAttribute(current, attribute: kAXRoleAttribute as String)
        if role == kAXTextFieldRole as String || role == kAXTextAreaRole as String
        {
            if setFocusAttribute(on: current) {
                usleep(40_000)
                return
            }
        } else if AXUIElementPerformAction(current, kAXPressAction as CFString) == .success {
            return
        }

        if let frame = frame(for: current) {
            try tap(
                at: CGPoint(x: frame.midX, y: frame.midY),
                durationMs: nil,
                targetPID: targetPID
            )
            usleep(80_000)
            if (role == kAXTextFieldRole as String || role == kAXTextAreaRole as String) &&
               setFocusAttribute(on: current)
            {
                usleep(40_000)
            }
            return
        }

        if AXUIElementPerformAction(current, kAXPressAction as CFString) == .success {
            return
        }
        candidate = elementAttribute(current, attribute: kAXParentAttribute as String)
    }
    throw DriverError.helper("failed to activate the matched accessibility element")
}

func outputJSON(_ object: Any) throws {
    let data = try JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted])
    guard let string = String(data: data, encoding: .utf8) else {
        throw DriverError.helper("failed to encode JSON output")
    }
    print(string)
}

func bridgeRequest(
    _ request: [String: Any],
    config: BridgeConfig
) throws {
    let requestID = UUID().uuidString
    let requestPath = URL(fileURLWithPath: config.directory)
        .appendingPathComponent("request-\(requestID).json")
    let responsePath = URL(fileURLWithPath: config.directory)
        .appendingPathComponent("response-\(requestID).json")

    var payload = request
    payload["requestId"] = requestID

    let data = try JSONSerialization.data(withJSONObject: payload, options: [])
    try data.write(to: requestPath)

    DistributedNotificationCenter.default().postNotificationName(
        Notification.Name(config.notificationName),
        object: nil,
        userInfo: ["requestId": requestID],
        deliverImmediately: true
    )

    let deadline = Date().addingTimeInterval(5)
    while Date() < deadline {
        if FileManager.default.fileExists(atPath: responsePath.path) {
            let responseData = try Data(contentsOf: responsePath)
            let response = try JSONDecoder().decode(BridgeResponse.self, from: responseData)
            try? FileManager.default.removeItem(at: requestPath)
            try? FileManager.default.removeItem(at: responsePath)
            if response.ok {
                return
            }
            throw DriverError.helper(response.error ?? "the injected macOS UI bridge rejected the request")
        }
        usleep(20_000)
    }

    throw DriverError.helper(
        "timed out waiting for the injected macOS UI bridge `\(config.notificationName)`"
    )
}

func pointElement(x: Double, y: Double) throws -> AXUIElement {
    let systemWide = AXUIElementCreateSystemWide()
    var resolved: AXUIElement?
    let error = AXUIElementCopyElementAtPosition(systemWide, Float(x), Float(y), &resolved)
    guard error == .success, let resolved else {
        throw DriverError.pointHitTest(x, y)
    }
    return resolved
}

func focusedElement(target: ApplicationTarget? = nil) -> AXUIElement? {
    if let target,
       let (_, application) = try? applicationElement(target: target),
       let value = attributeValue(application, attribute: kAXFocusedUIElementAttribute as String),
       CFGetTypeID(value) == AXUIElementGetTypeID()
    {
        return (value as! AXUIElement)
    }

    let systemWide = AXUIElementCreateSystemWide()
    var value: CFTypeRef?
    let error = AXUIElementCopyAttributeValue(
        systemWide,
        kAXFocusedUIElementAttribute as CFString,
        &value
    )
    guard error == .success,
          let value,
          CFGetTypeID(value) == AXUIElementGetTypeID()
    else {
        return nil
    }
    return (value as! AXUIElement)
}

func setStringAttribute(_ element: AXUIElement, attribute: String, value: String) -> Bool {
    var isSettable = DarwinBoolean(false)
    guard AXUIElementIsAttributeSettable(
        element,
        attribute as CFString,
        &isSettable
    ) == .success,
    isSettable.boolValue
    else {
        return false
    }

    return AXUIElementSetAttributeValue(
        element,
        attribute as CFString,
        value as CFTypeRef
    ) == .success
}

func setFocusAttribute(on element: AXUIElement) -> Bool {
    var isSettable = DarwinBoolean(false)
    guard AXUIElementIsAttributeSettable(
        element,
        kAXFocusedAttribute as CFString,
        &isSettable
    ) == .success,
    isSettable.boolValue
    else {
        return false
    }

    return AXUIElementSetAttributeValue(
        element,
        kAXFocusedAttribute as CFString,
        kCFBooleanTrue
    ) == .success
}

func elementAttribute(_ element: AXUIElement, attribute: String) -> AXUIElement? {
    guard let value = attributeValue(element, attribute: attribute),
          CFGetTypeID(value) == AXUIElementGetTypeID()
    else {
        return nil
    }
    return (value as! AXUIElement)
}

func editableElement(at point: CGPoint) throws -> AXUIElement {
    var candidate: AXUIElement? = try pointElement(x: point.x, y: point.y)
    for _ in 0..<6 {
        guard let element = candidate else {
            break
        }
        let role = stringAttribute(element, attribute: kAXRoleAttribute as String)
        if role == kAXTextFieldRole as String || role == kAXTextAreaRole as String {
            return element
        }
        candidate = elementAttribute(element, attribute: kAXParentAttribute as String)
    }
    throw DriverError.helper(
        "could not resolve an editable accessibility element at point (\(point.x), \(point.y))"
    )
}

func postMouseEvent(
    type: CGEventType,
    point: CGPoint,
    button: CGMouseButton = .left,
    clickState: Int64? = nil,
    pressure: Double? = nil,
    delta: CGPoint? = nil,
    source: CGEventSource? = nil,
    targetPID: pid_t? = nil
) throws {
    guard let event = CGEvent(
        mouseEventSource: source,
        mouseType: type,
        mouseCursorPosition: point,
        mouseButton: button
    ) else {
        throw DriverError.helper("failed to construct mouse event")
    }
    if let clickState {
        event.setIntegerValueField(.mouseEventClickState, value: clickState)
    }
    event.setIntegerValueField(.mouseEventButtonNumber, value: Int64(button.rawValue))
    if let pressure {
        event.setDoubleValueField(.mouseEventPressure, value: pressure)
    }
    if let delta {
        event.setIntegerValueField(.mouseEventDeltaX, value: Int64(delta.x.rounded()))
        event.setIntegerValueField(.mouseEventDeltaY, value: Int64(delta.y.rounded()))
    }
    if let targetPID {
        event.postToPid(targetPID)
    } else {
        event.post(tap: .cghidEventTap)
    }
}

func makeMouseEventSource() throws -> CGEventSource {
    guard let source = CGEventSource(stateID: .combinedSessionState) else {
        throw DriverError.helper("failed to construct a Quartz event source")
    }
    source.localEventsSuppressionInterval = 0
    return source
}

func tap(at point: CGPoint, durationMs: Int?, targetPID: pid_t? = nil) throws {
    try postMouseEvent(type: .mouseMoved, point: point, targetPID: targetPID)
    try postMouseEvent(type: .leftMouseDown, point: point, targetPID: targetPID)
    if let durationMs, durationMs > 0 {
        usleep(useconds_t(durationMs * 1000))
    }
    try postMouseEvent(type: .leftMouseUp, point: point, targetPID: targetPID)
}

func rightClick(at point: CGPoint, targetPID: pid_t? = nil) throws {
    try postMouseEvent(type: .mouseMoved, point: point, targetPID: targetPID)
    try postMouseEvent(type: .rightMouseDown, point: point, button: .right, targetPID: targetPID)
    usleep(40_000)
    try postMouseEvent(type: .rightMouseUp, point: point, button: .right, targetPID: targetPID)
}

func moveCursor(to point: CGPoint, targetPID: pid_t? = nil) throws {
    try postMouseEvent(type: .mouseMoved, point: point, targetPID: targetPID)
}

func swipe(
    from start: CGPoint,
    to end: CGPoint,
    durationMs: Int,
    delta: Int?,
    targetPID: pid_t? = nil
) throws {
    let distance = hypot(end.x - start.x, end.y - start.y)
    let stepSize = max(1.0, Double(delta ?? 6))
    let steps = max(2, Int(distance / stepSize))
    let sleepMicros = max(1, durationMs * 1000 / steps)
    let source = try makeMouseEventSource()

    try postMouseEvent(type: .mouseMoved, point: start, source: source, targetPID: targetPID)
    try postMouseEvent(
        type: .leftMouseDown,
        point: start,
        clickState: 1,
        pressure: 1,
        source: source,
        targetPID: targetPID
    )
    var previousPoint = start
    for step in 1...steps {
        let progress = Double(step) / Double(steps)
        let point = CGPoint(
            x: start.x + ((end.x - start.x) * progress),
            y: start.y + ((end.y - start.y) * progress)
        )
        let deltaPoint = CGPoint(
            x: point.x - previousPoint.x,
            y: point.y - previousPoint.y
        )
        try postMouseEvent(
            type: .leftMouseDragged,
            point: point,
            clickState: 1,
            pressure: 1,
            delta: deltaPoint,
            source: source,
            targetPID: targetPID
        )
        previousPoint = point
        usleep(useconds_t(sleepMicros))
    }
    try postMouseEvent(
        type: .leftMouseUp,
        point: end,
        clickState: 1,
        pressure: 0,
        source: source,
        targetPID: targetPID
    )
}

func drag(
    from start: CGPoint,
    to end: CGPoint,
    durationMs: Int,
    delta: Int?,
    targetPID: pid_t? = nil
) throws {
    let distance = hypot(end.x - start.x, end.y - start.y)
    let stepSize = max(1.0, Double(delta ?? 6))
    let steps = max(2, Int(distance / stepSize))
    let sleepMicros = max(1, durationMs * 1000 / steps)
    let source = try makeMouseEventSource()

    try postMouseEvent(type: .mouseMoved, point: start, source: source, targetPID: targetPID)
    try postMouseEvent(
        type: .leftMouseDown,
        point: start,
        clickState: 1,
        pressure: 1,
        source: source,
        targetPID: targetPID
    )
    usleep(80_000)
    var previousPoint = start
    for step in 1...steps {
        let progress = Double(step) / Double(steps)
        let point = CGPoint(
            x: start.x + ((end.x - start.x) * progress),
            y: start.y + ((end.y - start.y) * progress)
        )
        let deltaPoint = CGPoint(
            x: point.x - previousPoint.x,
            y: point.y - previousPoint.y
        )
        try postMouseEvent(
            type: .leftMouseDragged,
            point: point,
            clickState: 1,
            pressure: 1,
            delta: deltaPoint,
            source: source,
            targetPID: targetPID
        )
        previousPoint = point
        usleep(useconds_t(sleepMicros))
    }
    usleep(80_000)
    try postMouseEvent(
        type: .leftMouseUp,
        point: end,
        clickState: 1,
        pressure: 0,
        source: source,
        targetPID: targetPID
    )
}

func scroll(direction: String, point: CGPoint? = nil, targetPID: pid_t? = nil) throws {
    let (vertical, horizontal): (Int32, Int32)
    switch direction.lowercased() {
    case "up":
        vertical = 500
        horizontal = 0
    case "down":
        vertical = -500
        horizontal = 0
    case "left":
        vertical = 0
        horizontal = -500
    case "right":
        vertical = 0
        horizontal = 500
    default:
        throw DriverError.helper("unsupported scroll direction `\(direction)`")
    }

    if let point {
        try postMouseEvent(type: .mouseMoved, point: point, targetPID: targetPID)
    }

    guard let event = CGEvent(
        scrollWheelEvent2Source: nil,
        units: .pixel,
        wheelCount: 2,
        wheel1: vertical,
        wheel2: horizontal,
        wheel3: 0
    ) else {
        throw DriverError.helper("failed to construct scroll event")
    }
    if let targetPID {
        event.postToPid(targetPID)
    } else {
        event.post(tap: .cghidEventTap)
    }
}

func inputText(_ text: String, target: ApplicationTarget?) throws {
    var targetPID: pid_t?
    if let target {
        targetPID = try targetProcessIdentifier(target: target)
    }

    if let element = focusedElement(target: target) {
        let role = stringAttribute(element, attribute: kAXRoleAttribute as String)
        if role == kAXTextFieldRole as String || role == kAXTextAreaRole as String {
            try typeTextEvents(text, targetPID: targetPID)
            return
        }

        let current = stringAttribute(element, attribute: kAXValueAttribute as String) ?? ""
        if setStringAttribute(element, attribute: kAXValueAttribute as String, value: current + text) {
            return
        }
    }

    try typeTextEvents(text, targetPID: targetPID)
}

struct KeyboardModifiers {
    var keyCodes: [CGKeyCode]
    var flags: CGEventFlags
}

func keyboardModifiers(from arguments: [String]) throws -> KeyboardModifiers {
    guard let raw = try optionalFlag("--modifiers", in: arguments)?
        .trimmingCharacters(in: .whitespacesAndNewlines),
        !raw.isEmpty
    else {
        return KeyboardModifiers(keyCodes: [], flags: [])
    }

    return try raw.split(separator: ",").reduce(
        into: KeyboardModifiers(keyCodes: [], flags: [])
    ) {
        modifiers,
        entry in
        let token = String(entry).trimmingCharacters(in: .whitespacesAndNewlines)
        switch token.lowercased() {
        case "command":
            modifiers.keyCodes.append(55)
            modifiers.flags.insert(.maskCommand)
        case "shift":
            modifiers.keyCodes.append(56)
            modifiers.flags.insert(.maskShift)
        case "option":
            modifiers.keyCodes.append(58)
            modifiers.flags.insert(.maskAlternate)
        case "control":
            modifiers.keyCodes.append(59)
            modifiers.flags.insert(.maskControl)
        case "function":
            modifiers.keyCodes.append(63)
            modifiers.flags.insert(.maskSecondaryFn)
        case "":
            break
        default:
            throw DriverError.helper("unsupported keyboard modifier `\(token)`")
        }
    }
}

func postKeyEvent(_ keyCode: CGKeyCode, keyDown: Bool, targetPID: pid_t? = nil) throws {
    guard let event = CGEvent(keyboardEventSource: nil, virtualKey: keyCode, keyDown: keyDown)
    else {
        throw DriverError.helper("failed to construct keyboard event")
    }
    if let targetPID {
        event.postToPid(targetPID)
    } else {
        event.post(tap: .cghidEventTap)
    }
}

func pressKeyCode(
    keyCode: Int,
    durationMs: Int?,
    modifiers: KeyboardModifiers,
    targetPID: pid_t? = nil
) throws {
    for modifier in modifiers.keyCodes {
        try postKeyEvent(modifier, keyDown: true, targetPID: targetPID)
    }

    guard let keyDown = CGEvent(
        keyboardEventSource: nil,
        virtualKey: CGKeyCode(keyCode),
        keyDown: true
    ),
        let keyUp = CGEvent(
            keyboardEventSource: nil,
            virtualKey: CGKeyCode(keyCode),
            keyDown: false
        )
    else {
        throw DriverError.helper("failed to construct keyboard event")
    }
    keyDown.flags = modifiers.flags
    keyUp.flags = modifiers.flags
    if let targetPID {
        keyDown.postToPid(targetPID)
    } else {
        keyDown.post(tap: .cghidEventTap)
    }
    if let durationMs, durationMs > 0 {
        usleep(useconds_t(durationMs * 1000))
    }
    if let targetPID {
        keyUp.postToPid(targetPID)
    } else {
        keyUp.post(tap: .cghidEventTap)
    }

    for modifier in modifiers.keyCodes.reversed() {
        try postKeyEvent(modifier, keyDown: false, targetPID: targetPID)
    }
}

func postUnicodeEvent(_ text: String, keyDown: Bool, targetPID: pid_t? = nil) throws {
    guard let event = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: keyDown) else {
        throw DriverError.helper("failed to construct unicode keyboard event")
    }

    let utf16 = Array(text.utf16)
    utf16.withUnsafeBufferPointer { buffer in
        guard let baseAddress = buffer.baseAddress else { return }
        event.keyboardSetUnicodeString(stringLength: buffer.count, unicodeString: baseAddress)
    }

    if let targetPID {
        event.postToPid(targetPID)
    } else {
        event.post(tap: .cghidEventTap)
    }
}

func typeTextEvents(_ text: String, targetPID: pid_t? = nil) throws {
    for character in text {
        let scalar = String(character)
        try postUnicodeEvent(scalar, keyDown: true, targetPID: targetPID)
        try postUnicodeEvent(scalar, keyDown: false, targetPID: targetPID)
        usleep(8_000)
    }
}

func setValueAtPoint(_ point: CGPoint, text: String) throws {
    let element = try editableElement(at: point)
    let current = stringAttribute(element, attribute: kAXValueAttribute as String) ?? ""
    guard setStringAttribute(
        element,
        attribute: kAXValueAttribute as String,
        value: current + text
    ) else {
        throw DriverError.helper(
            "failed to set text on the accessibility element at point (\(point.x), \(point.y))"
        )
    }
}

func normalizedMenuLabel(_ value: String) -> String {
    value.trimmingCharacters(in: .whitespacesAndNewlines)
}

func menuChildren(of element: AXUIElement) -> [AXUIElement] {
    var seen = Set<CFHashCode>()
    var results = [AXUIElement]()

    func append(_ candidate: AXUIElement) {
        let hash = CFHash(candidate)
        guard seen.insert(hash).inserted else {
            return
        }
        results.append(candidate)
    }

    for attribute in [
        kAXChildrenAttribute as String,
        kAXVisibleChildrenAttribute as String,
        kAXMenuBarAttribute as String,
        "AXMenu",
    ] {
        guard let value = attributeValue(element, attribute: attribute) else {
            continue
        }
        let typeID = CFGetTypeID(value)
        if typeID == AXUIElementGetTypeID() {
            append(value as! AXUIElement)
            continue
        }
        if typeID == CFArrayGetTypeID(), let array = value as? [Any] {
            for entry in array {
                if CFGetTypeID(entry as CFTypeRef) == AXUIElementGetTypeID() {
                    append(entry as! AXUIElement)
                }
            }
        }
    }

    return results
}

func menuLabelMatches(_ element: AXUIElement, target: String) -> Bool {
    let normalizedTarget = normalizedMenuLabel(target)
    guard !normalizedTarget.isEmpty else {
        return false
    }

    for candidate in [
        stringAttribute(element, attribute: kAXTitleAttribute as String),
        stringAttribute(element, attribute: kAXDescriptionAttribute as String),
        attributeValue(element, attribute: kAXValueAttribute as String).flatMap(stringValue),
    ].compactMap({ $0 }).map(normalizedMenuLabel) {
        if candidate == normalizedTarget {
            return true
        }
    }
    return false
}

func findMenuItem(named label: String, in container: AXUIElement) -> AXUIElement? {
    let directChildren = menuChildren(of: container)
    if let directMatch = directChildren.first(where: { menuLabelMatches($0, target: label) }) {
        return directMatch
    }

    for child in directChildren {
        let nestedChildren = menuChildren(of: child)
        if let nestedMatch = nestedChildren.first(where: { menuLabelMatches($0, target: label) }) {
            return nestedMatch
        }
    }
    return nil
}

func menuBarElement(for application: AXUIElement) -> AXUIElement? {
    if let menuBar = elementAttribute(application, attribute: kAXMenuBarAttribute as String) {
        return menuBar
    }

    return childElements(of: application).first {
        stringAttribute($0, attribute: kAXRoleAttribute as String) == kAXMenuBarRole as String
    }
}

func bringApplicationToFront(target: ApplicationTarget) throws {
    let application = try runningApplication(target: target)

    // Keyboard shortcuts must target the actual AUT, not whichever desktop app currently owns
    // the active menu bar. Under Codex, a plain `activateAllWindows` is sometimes too weak and
    // the shortcut lands in the host app instead.
    _ = application.activate(options: [.activateAllWindows])

    let deadline = Date().addingTimeInterval(2.0)
    while Date() < deadline {
        if let frontmost = NSWorkspace.shared.frontmostApplication {
            switch target {
            case .bundleID(let bundleID):
                if frontmost.bundleIdentifier == bundleID {
                    return
                }
            case .pid(let pid):
                if frontmost.processIdentifier == pid {
                    return
                }
            }
        }
        usleep(50_000)
    }
}

func selectMenuItem(target: ApplicationTarget, path: [String]) throws {
    let labels = path.map(normalizedMenuLabel).filter { !$0.isEmpty }
    guard !labels.isEmpty else {
        throw DriverError.helper("`selectMenuItem` requires at least one menu label")
    }

    let (_, applicationElementRef) = try applicationElement(target: target)
    guard let menuBar = menuBarElement(for: applicationElementRef) else {
        throw DriverError.helper("failed to resolve the menu bar for `\(target.description)`")
    }

    var container = menuBar
    for (index, label) in labels.enumerated() {
        guard let target = findMenuItem(named: label, in: container) else {
            throw DriverError.helper("failed to resolve menu item `\(label)`")
        }
        guard AXUIElementPerformAction(target, kAXPressAction as CFString) == .success else {
            throw DriverError.helper("failed to activate menu item `\(label)`")
        }
        if index + 1 < labels.count {
            usleep(150_000)
            container = target
        }
    }
}

func writeWindowScreenshot(target: ApplicationTarget, outputPath: String) throws {
    let captureInfo = try windowCaptureInfo(target: target)
    guard #available(macOS 14.0, *) else {
        throw DriverError.helper("macOS window screenshots require macOS 14.0 or newer")
    }

    let contentSemaphore = DispatchSemaphore(value: 0)
    var shareableContent: SCShareableContent?
    var shareableContentError: Error?
    SCShareableContent.getExcludingDesktopWindows(true, onScreenWindowsOnly: true) {
        content,
        error in
        shareableContent = content
        shareableContentError = error
        contentSemaphore.signal()
    }
    contentSemaphore.wait()

    if let shareableContentError {
        throw shareableContentError
    }

    guard let shareableContent else {
        throw DriverError.helper("failed to query macOS shareable content")
    }

    let targetWindow = shareableContent.windows.first { window in
        window.windowID == CGWindowID(captureInfo.windowNumber)
    } ?? shareableContent.windows.first { window in
        switch target {
        case .bundleID(let bundleID):
            return window.owningApplication?.bundleIdentifier == bundleID
                && intersectionArea(window.frame, captureInfo.frame) > 0
        case .pid(let pid):
            return window.owningApplication?.processID == pid
                && intersectionArea(window.frame, captureInfo.frame) > 0
        }
    }
    guard let targetWindow else {
        throw DriverError.helper("failed to resolve the macOS window for screenshot capture")
    }

    let filter = SCContentFilter(desktopIndependentWindow: targetWindow)
    let configuration = SCStreamConfiguration()
    let scale = max(1.0, Double(filter.pointPixelScale))
    configuration.width = max(1, Int(targetWindow.frame.width * scale))
    configuration.height = max(1, Int(targetWindow.frame.height * scale))
    configuration.showsCursor = false

    let imageSemaphore = DispatchSemaphore(value: 0)
    var capturedImage: CGImage?
    var captureError: Error?
    SCScreenshotManager.captureImage(contentFilter: filter, configuration: configuration) {
        image,
        error in
        capturedImage = image
        captureError = error
        imageSemaphore.signal()
    }
    imageSemaphore.wait()

    if let captureError {
        throw captureError
    }
    guard let capturedImage else {
        throw DriverError.helper("failed to capture macOS window image")
    }

    let bitmap = NSBitmapImageRep(cgImage: capturedImage)
    guard let pngData = bitmap.representation(using: .png, properties: [:]) else {
        throw DriverError.helper("failed to encode macOS window screenshot")
    }
    try pngData.write(to: URL(fileURLWithPath: outputPath))
}

func run() throws {
    var arguments = Array(CommandLine.arguments.dropFirst())
    guard !arguments.isEmpty else {
        throw DriverError.usage("usage: orbi-macos-ui-driver <command> [options]")
    }

    let command = arguments.removeFirst()

    if command == "doctor" {
        try outputJSON(
            DoctorStatus(
                accessibilityTrusted: AXIsProcessTrusted(),
                screenCaptureAccess: CGPreflightScreenCaptureAccess()
            ).dictionary
        )
        return
    }

    if command == "frontmost-application" {
        try outputJSON(frontmostApplicationInfo()?.dictionary ?? [:])
        return
    }

    if command == "launch-app" {
        let appPath = try requireFlag("--app-path", in: arguments)
        let launchArguments = try optionalRepeatedFlags("--argument", in: arguments)
        let launchEnvironment = try optionalRepeatedFlags("--env", in: arguments)
        try outputJSON(
            launchApplication(
                appPath: appPath,
                arguments: launchArguments,
                environmentEntries: launchEnvironment
            ).dictionary
        )
        return
    }

    if command == "reopen-app" {
        let target = try requireApplicationTarget(in: arguments)
        try reopenApplication(target: target)
        return
    }

    try ensureAccessibilityPermission()

    switch command {
    case "describe-all":
        let target = try requireApplicationTarget(in: arguments)
        let (_, element) = try applicationElement(target: target)
        try outputJSON(collectSnapshots(from: element))

    case "window-info":
        let target = try requireApplicationTarget(in: arguments)
        try outputJSON(windowCaptureInfo(target: target).dictionary)

    case "describe-point":
        let x = try requireDoubleFlag("--x", in: arguments)
        let y = try requireDoubleFlag("--y", in: arguments)
        let element = try pointElement(x: x, y: y)
        try outputJSON(serialize(element).dictionary)

    case "focus":
        let target = try requireApplicationTarget(in: arguments)
        try bringApplicationToFront(target: target)

    case "tap":
        let x = try requireDoubleFlag("--x", in: arguments)
        let y = try requireDoubleFlag("--y", in: arguments)
        let durationMs = try optionalIntFlag("--duration-ms", in: arguments)
        if let bridge = try optionalBridgeConfig(in: arguments) {
            try bridgeRequest(
                [
                    "command": "tap",
                    "x": x,
                    "y": y,
                    "durationMs": durationMs ?? NSNull(),
                ],
                config: bridge
            )
            return
        }
        let targetPID = try optionalTargetPID(in: arguments)
        try tap(at: CGPoint(x: x, y: y), durationMs: durationMs, targetPID: targetPID)

    case "activate-element":
        let target = try requireApplicationTarget(in: arguments)
        let identifier = try optionalFlag("--id", in: arguments)
        let text = try optionalFlag("--text", in: arguments)
        guard identifier != nil || text != nil else {
            throw DriverError.usage("activate-element requires --id and/or --text")
        }
        let (_, element) = try applicationElement(target: target)
        let targetPID = try targetProcessIdentifier(target: target)
        guard let match = bestMatchingElement(root: element, text: text, identifier: identifier) else {
            throw DriverError.helper("failed to resolve a matching accessibility element")
        }
        try pressElementOrAncestor(match, targetPID: targetPID)

    case "move":
        let x = try requireDoubleFlag("--x", in: arguments)
        let y = try requireDoubleFlag("--y", in: arguments)
        if let bridge = try optionalBridgeConfig(in: arguments) {
            try bridgeRequest(
                [
                    "command": "move",
                    "x": x,
                    "y": y,
                ],
                config: bridge
            )
            return
        }
        let targetPID = try optionalTargetPID(in: arguments)
        try moveCursor(to: CGPoint(x: x, y: y), targetPID: targetPID)

    case "right-click":
        let x = try requireDoubleFlag("--x", in: arguments)
        let y = try requireDoubleFlag("--y", in: arguments)
        if let bridge = try optionalBridgeConfig(in: arguments) {
            try bridgeRequest(
                [
                    "command": "right-click",
                    "x": x,
                    "y": y,
                ],
                config: bridge
            )
            return
        }
        let targetPID = try optionalTargetPID(in: arguments)
        try rightClick(at: CGPoint(x: x, y: y), targetPID: targetPID)

    case "swipe":
        let startX = try requireDoubleFlag("--start-x", in: arguments)
        let startY = try requireDoubleFlag("--start-y", in: arguments)
        let endX = try requireDoubleFlag("--end-x", in: arguments)
        let endY = try requireDoubleFlag("--end-y", in: arguments)
        let durationMs = try optionalIntFlag("--duration-ms", in: arguments) ?? 500
        let delta = try optionalIntFlag("--delta", in: arguments)
        let targetPID = try optionalTargetPID(in: arguments)
        try swipe(
            from: CGPoint(x: startX, y: startY),
            to: CGPoint(x: endX, y: endY),
            durationMs: durationMs,
            delta: delta,
            targetPID: targetPID
        )

    case "drag":
        let startX = try requireDoubleFlag("--start-x", in: arguments)
        let startY = try requireDoubleFlag("--start-y", in: arguments)
        let endX = try requireDoubleFlag("--end-x", in: arguments)
        let endY = try requireDoubleFlag("--end-y", in: arguments)
        let durationMs = try optionalIntFlag("--duration-ms", in: arguments) ?? 650
        let delta = try optionalIntFlag("--delta", in: arguments)
        if let bridge = try optionalBridgeConfig(in: arguments) {
            try bridgeRequest(
                [
                    "command": "drag",
                    "startX": startX,
                    "startY": startY,
                    "endX": endX,
                    "endY": endY,
                    "durationMs": durationMs,
                    "delta": delta ?? NSNull(),
                ],
                config: bridge
            )
            return
        }
        let targetPID = try optionalTargetPID(in: arguments)
        try drag(
            from: CGPoint(x: startX, y: startY),
            to: CGPoint(x: endX, y: endY),
            durationMs: durationMs,
            delta: delta,
            targetPID: targetPID
        )

    case "scroll":
        let direction = try requireFlag("--direction", in: arguments)
        if let bridge = try optionalBridgeConfig(in: arguments) {
            let x = try requireDoubleFlag("--x", in: arguments)
            let y = try requireDoubleFlag("--y", in: arguments)
            try bridgeRequest(
                [
                    "command": "scroll",
                    "direction": direction,
                    "x": x,
                    "y": y,
                ],
                config: bridge
            )
            return
        }
        let targetPID = try optionalTargetPID(in: arguments)
        let point: CGPoint?
        if let x = try optionalFlag("--x", in: arguments),
           let y = try optionalFlag("--y", in: arguments),
           let xValue = Double(x),
           let yValue = Double(y)
        {
            point = CGPoint(x: xValue, y: yValue)
        } else {
            point = nil
        }
        try scroll(direction: direction, point: point, targetPID: targetPID)

    case "scroll-at-point":
        let x = try requireDoubleFlag("--x", in: arguments)
        let y = try requireDoubleFlag("--y", in: arguments)
        let direction = try requireFlag("--direction", in: arguments)
        if let bridge = try optionalBridgeConfig(in: arguments) {
            try bridgeRequest(
                [
                    "command": "scroll",
                    "direction": direction,
                    "x": x,
                    "y": y,
                ],
                config: bridge
            )
            return
        }
        let targetPID = try optionalTargetPID(in: arguments)
        try scroll(direction: direction, point: CGPoint(x: x, y: y), targetPID: targetPID)

    case "bridge-ping":
        let bridge = try optionalBridgeConfig(in: arguments)
            ?? { throw DriverError.usage("bridge-ping requires --bridge-dir and --bridge-name") }()
        try bridgeRequest(["command": "ping"], config: bridge)

    case "text":
        let text = try requireFlag("--text", in: arguments)
        let target = try optionalApplicationTarget(in: arguments)
        try inputText(text, target: target)

    case "set-value-at-point":
        let x = try requireDoubleFlag("--x", in: arguments)
        let y = try requireDoubleFlag("--y", in: arguments)
        let text = try requireFlag("--text", in: arguments)
        try setValueAtPoint(CGPoint(x: x, y: y), text: text)

    case "key":
        let target = try requireApplicationTarget(in: arguments)
        let keyCode = try requireIntFlag("--keycode", in: arguments)
        let durationMs = try optionalIntFlag("--duration-ms", in: arguments)
        let modifiers = try keyboardModifiers(from: arguments)
        let targetPID = try targetProcessIdentifier(target: target)
        try pressKeyCode(
            keyCode: keyCode,
            durationMs: durationMs,
            modifiers: modifiers,
            targetPID: targetPID
        )

    case "menu-item":
        let target = try requireApplicationTarget(in: arguments)
        let items = try repeatedFlags("--item", in: arguments)
        try selectMenuItem(target: target, path: items)

    case "screenshot-window":
        let target = try requireApplicationTarget(in: arguments)
        let outputPath = try requireFlag("--output", in: arguments)
        try writeWindowScreenshot(target: target, outputPath: outputPath)

    default:
        throw DriverError.usage("unsupported command `\(command)`")
    }
}

do {
    try run()
} catch {
    fail(error)
}
