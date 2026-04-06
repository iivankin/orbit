import Foundation
import SwiftLintFramework

private struct LintRequest: Decodable {
    struct CompilerInvocation: Decodable {
        let arguments: [String]
        let sourceFiles: [String]
    }

    let workingDirectory: String
    let configurationJson: String?
    let files: [String]
    let compilerInvocations: [CompilerInvocation]
}

private struct PassResult {
    let violations: [StyleViolation]
    let seriousViolations: Int
}

private struct ResolvedLintConfiguration {
    let configuration: Configuration
    let severityOverrides: [String: ViolationSeverity]
}

private struct RuleDescriptor {
    let canonicalID: String
    let isOptIn: Bool
    let isAnalyzer: Bool
}

private struct ParsedRuleSetting {
    let enabled: Bool
    let configurationValue: Any?
    let severityOverride: ViolationSeverity?
}

private enum LintToolError: LocalizedError {
    case usage
    case invalidWorkingDirectory(String)
    case invalidConfigurationPayload

    var errorDescription: String? {
        switch self {
        case .usage:
            return "usage: orbit-swiftlint <request.json>"
        case let .invalidWorkingDirectory(path):
            return "failed to change directory to \(path)"
        case .invalidConfigurationPayload:
            return "Orbit lint configuration must be a JSON object"
        }
    }
}

public func orbitSwiftLintMain(arguments: [String] = CommandLine.arguments) -> Int32 {
    do {
        return try OrbitSwiftLintTool.run(arguments: arguments)
    } catch {
        writeToStandardError("error: \(error.localizedDescription)\n")
        return 1
    }
}

@_cdecl("orbit_swiftlint_run_request")
public func orbit_swiftlint_run_request(requestPath: UnsafePointer<CChar>?) -> Int32 {
    guard let requestPath else {
        writeToStandardError("error: missing request path for orbit-swiftlint\n")
        return 1
    }
    return orbitSwiftLintMain(arguments: ["orbit-swiftlint", String(cString: requestPath)])
}

private enum OrbitSwiftLintTool {
    static func run(arguments: [String]) throws -> Int32 {
        guard arguments.count == 2 else {
            throw LintToolError.usage
        }

        let requestPath = arguments[1]
        if let status = try maybeHandleMockRequest(at: requestPath) {
            return status
        }

        let request = try decodeRequest(at: requestPath)
        guard FileManager.default.changeCurrentDirectoryPath(request.workingDirectory) else {
            throw LintToolError.invalidWorkingDirectory(request.workingDirectory)
        }

        registerRules()
        let resolvedConfiguration = try loadConfiguration(from: request.configurationJson)
        let compilerArgumentsByFile = buildCompilerArgumentsMap(from: request.compilerInvocations)
        let reporter = reporterFrom(identifier: resolvedConfiguration.configuration.reporter)

        let syntaxResult = try executePass(
            files: request.files,
            configuration: resolvedConfiguration.configuration,
            compilerArgumentsByFile: nil,
            severityOverrides: resolvedConfiguration.severityOverrides
        )
        emit(report: reporter.generateReport(syntaxResult.violations))

        let semanticFiles = request.files.filter { compilerArgumentsByFile[$0] != nil }
        let semanticResult = try executePass(
            files: semanticFiles,
            configuration: resolvedConfiguration.configuration,
            compilerArgumentsByFile: compilerArgumentsByFile,
            severityOverrides: resolvedConfiguration.severityOverrides
        )
        emit(report: reporter.generateReport(semanticResult.violations))

        if syntaxResult.seriousViolations + semanticResult.seriousViolations > 0 {
            return 2
        }
        return 0
    }

    private static func decodeRequest(at path: String) throws -> LintRequest {
        let data = try Data(contentsOf: URL(fileURLWithPath: path))
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(LintRequest.self, from: data)
    }

    private static func registerRules() {
        // SwiftLintFramework doesn't publicly expose its internal registration helper,
        // so Orbit registers the public rule sets it can see up front.
        RuleRegistry.shared.register(rules: builtInRules + coreRules)
    }

    private static func loadConfiguration(from json: String?) throws -> ResolvedLintConfiguration {
        guard let json else {
            return ResolvedLintConfiguration(
                configuration: Configuration(configurationFiles: []),
                severityOverrides: [:]
            )
        }

        let payload = try JSONSerialization.jsonObject(with: Data(json.utf8))
        guard let orbitConfiguration = payload as? [String: Any] else {
            throw LintToolError.invalidConfigurationPayload
        }

        let translated = translateOrbitConfiguration(orbitConfiguration)
        return ResolvedLintConfiguration(
            configuration: try Configuration(dict: translated.configurationDictionary),
            severityOverrides: translated.severityOverrides
        )
    }

    private static func buildCompilerArgumentsMap(
        from invocations: [LintRequest.CompilerInvocation]
    ) -> [String: [String]] {
        var commands = [String: [String]]()
        for invocation in invocations {
            let filteredArguments = filterCompilerArguments(invocation.arguments)
            for sourceFile in invocation.sourceFiles {
                let normalizedPath = normalizePath(sourceFile)
                if commands[normalizedPath] == nil {
                    commands[normalizedPath] = filteredArguments
                }
            }
        }
        return commands
    }

    private static func executePass(
        files: [String],
        configuration: Configuration,
        compilerArgumentsByFile: [String: [String]]?,
        severityOverrides: [String: ViolationSeverity]
    ) throws -> PassResult {
        guard !files.isEmpty else {
            return PassResult(violations: [], seriousViolations: 0)
        }

        let baseline = try loadBaseline(from: configuration)
        let storage = RuleStorage()
        var collectedLinters = [CollectedLinter]()

        for path in files {
            let normalizedPath = normalizePath(path)
            let file = SwiftLintFile(pathDeferringReading: normalizedPath)
            let fileConfiguration = configuration.configuration(for: file)

            if let compilerArgumentsByFile {
                guard let compilerArguments = compilerArgumentsByFile[normalizedPath] else {
                    continue
                }
                let linter = Linter(
                    file: file,
                    configuration: fileConfiguration,
                    compilerArguments: compilerArguments
                )
                collectedLinters.append(linter.collect(into: storage))
            } else {
                let linter = Linter(file: file, configuration: fileConfiguration)
                collectedLinters.append(linter.collect(into: storage))
            }
        }

        var violations = [StyleViolation]()
        for linter in collectedLinters {
            let passViolations = applySeverityOverrides(
                violations: applyLeniency(
                    violations: linter.styleViolations(using: storage),
                    strict: configuration.strict,
                    lenient: configuration.lenient
                ),
                severityOverrides: severityOverrides
            )
            violations.append(contentsOf: baseline?.filter(passViolations) ?? passViolations)
        }

        if isWarningThresholdBroken(configuration: configuration, violations: violations),
           !configuration.lenient,
           let threshold = configuration.warningThreshold {
            violations.append(createThresholdViolation(threshold: threshold))
        }

        let seriousViolations = violations.filter { $0.severity == .error }.count
        return PassResult(violations: violations, seriousViolations: seriousViolations)
    }

    private static func loadBaseline(from configuration: Configuration) throws -> Baseline? {
        guard let path = configuration.baseline else {
            return nil
        }

        do {
            return try Baseline(fromPath: path)
        } catch {
            Issue.baselineNotReadable(path: path).print()
            throw error
        }
    }

    private static func isWarningThresholdBroken(
        configuration: Configuration,
        violations: [StyleViolation]
    ) -> Bool {
        guard let warningThreshold = configuration.warningThreshold else {
            return false
        }
        let warningCount = violations.filter { $0.severity == .warning }.count
        return warningCount >= warningThreshold
    }

    private static func createThresholdViolation(threshold: Int) -> StyleViolation {
        let description = RuleDescription(
            identifier: "warning_threshold",
            name: "Warning Threshold",
            description: "Number of warnings thrown is above the threshold",
            kind: .lint
        )
        return StyleViolation(
            ruleDescription: description,
            severity: .error,
            location: Location(file: "", line: 0, character: 0),
            reason: "Number of warnings exceeded threshold of \(threshold)."
        )
    }

    private static func applyLeniency(
        violations: [StyleViolation],
        strict: Bool,
        lenient: Bool
    ) -> [StyleViolation] {
        switch (strict, lenient) {
        case (false, false):
            return violations
        case (false, true):
            return violations.map { violation in
                violation.severity == .error ? violation.with(severity: .warning) : violation
            }
        case (true, false):
            return violations.map { violation in
                violation.severity == .warning ? violation.with(severity: .error) : violation
            }
        case (true, true):
            return violations
        }
    }

    private static func applySeverityOverrides(
        violations: [StyleViolation],
        severityOverrides: [String: ViolationSeverity]
    ) -> [StyleViolation] {
        violations.map { violation in
            guard let severity = severityOverrides[violation.ruleIdentifier] else {
                return violation
            }
            return violation.with(severity: severity)
        }
    }

    private static func translateOrbitConfiguration(
        _ orbitConfiguration: [String: Any]
    ) -> (configurationDictionary: [String: Any], severityOverrides: [String: ViolationSeverity]) {
        var translated = [String: Any]()
        var disabledRules = Set<String>()
        var optInRules = Set<String>()
        var analyzerRules = Set<String>()
        var severityOverrides = [String: ViolationSeverity]()
        let descriptors = ruleDescriptors()

        for (key, value) in orbitConfiguration {
            guard let descriptor = descriptors[key] else {
                translated[key] = value
                continue
            }

            let parsed = parseRuleSetting(value)
            if !parsed.enabled {
                disabledRules.insert(descriptor.canonicalID)
                optInRules.remove(descriptor.canonicalID)
                analyzerRules.remove(descriptor.canonicalID)
                severityOverrides.removeValue(forKey: descriptor.canonicalID)
                continue
            }

            if descriptor.isAnalyzer {
                analyzerRules.insert(descriptor.canonicalID)
            } else if descriptor.isOptIn {
                optInRules.insert(descriptor.canonicalID)
            }

            if let configurationValue = parsed.configurationValue {
                translated[descriptor.canonicalID] = configurationValue
            }
            if let severityOverride = parsed.severityOverride {
                severityOverrides[descriptor.canonicalID] = severityOverride
            }
        }

        if !disabledRules.isEmpty {
            translated["disabled_rules"] = Array(disabledRules).sorted()
        }
        if !optInRules.isEmpty {
            translated["opt_in_rules"] = Array(optInRules).sorted()
        }
        if !analyzerRules.isEmpty {
            translated["analyzer_rules"] = Array(analyzerRules).sorted()
        }

        return (translated, severityOverrides)
    }

    private static func ruleDescriptors() -> [String: RuleDescriptor] {
        var descriptors = [String: RuleDescriptor]()
        for (canonicalID, ruleType) in RuleRegistry.shared.list.list {
            let rule = ruleType.init()
            let descriptor = RuleDescriptor(
                canonicalID: canonicalID,
                isOptIn: rule is any OptInRule,
                isAnalyzer: rule is any AnalyzerRule
            )
            for identifier in ruleType.description.allIdentifiers {
                descriptors[identifier] = descriptor
            }
        }
        return descriptors
    }

    private static func parseRuleSetting(_ value: Any) -> ParsedRuleSetting {
        if value is NSNull {
            return ParsedRuleSetting(enabled: false, configurationValue: nil, severityOverride: nil)
        }
        if let bool = value as? Bool {
            return ParsedRuleSetting(enabled: bool, configurationValue: nil, severityOverride: nil)
        }
        if let string = value as? String {
            if let severity = severityOverride(from: string) {
                return ParsedRuleSetting(
                    enabled: normalizeLevel(string) != "off",
                    configurationValue: nil,
                    severityOverride: severity
                )
            }
            if normalizeLevel(string) == "off" {
                return ParsedRuleSetting(enabled: false, configurationValue: nil, severityOverride: nil)
            }
            return ParsedRuleSetting(enabled: true, configurationValue: string, severityOverride: nil)
        }
        if let array = value as? [Any], let first = array.first as? String {
            let normalizedLevel = normalizeLevel(first)
            if normalizedLevel == "off" {
                return ParsedRuleSetting(enabled: false, configurationValue: nil, severityOverride: nil)
            }
            if let severity = severityOverride(from: first) {
                return ParsedRuleSetting(
                    enabled: true,
                    configurationValue: array.count > 1 ? array[1] : nil,
                    severityOverride: severity
                )
            }
        }
        return ParsedRuleSetting(enabled: true, configurationValue: value, severityOverride: nil)
    }

    private static func severityOverride(from level: String) -> ViolationSeverity? {
        switch normalizeLevel(level) {
        case "warn":
            return .warning
        case "error":
            return .error
        default:
            return nil
        }
    }

    private static func normalizeLevel(_ level: String) -> String {
        switch level.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() {
        case "warning":
            return "warn"
        case "err":
            return "error"
        default:
            return level.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        }
    }

    private static func emit(report: String) {
        guard !report.isEmpty else {
            return
        }
        writeToStandardOutput(report)
        writeToStandardOutput("\n")
    }
}

private func maybeHandleMockRequest(at requestPath: String) throws -> Int32? {
    guard let mockLogPath = ProcessInfo.processInfo.environment["MOCK_LOG"] else {
        return nil
    }

    let requestBody = try String(contentsOfFile: requestPath, encoding: .utf8)
    appendToMockLog(
        mockLogPath,
        lines: [
            "orbit-swiftlint \(requestPath)\n",
            "orbit-swiftlint request:\n",
            requestBody,
            "\n",
        ]
    )
    return 0
}

private func normalizePath(_ path: String) -> String {
    URL(fileURLWithPath: path).standardizedFileURL.path
}

private func filterCompilerArguments(_ arguments: [String]) -> [String] {
    var filtered = arguments
    if filtered.first == "swiftc" {
        filtered.removeFirst()
    }

    filtered = filtered.map { argument in
        argument
            .replacingOccurrences(of: "\\=", with: "=")
            .replacingOccurrences(of: "\\ ", with: " ")
    }
    filtered.append(contentsOf: ["-D", "DEBUG"])

    while let flagIndex = filtered.firstIndex(of: "-output-file-map"), flagIndex + 1 < filtered.count {
        filtered.removeSubrange(flagIndex...(flagIndex + 1))
    }

    return filtered
        .filter { argument in
            ![
                "-parseable-output",
                "-incremental",
                "-serialize-diagnostics",
                "-emit-dependencies",
                "-use-frontend-parseable-output",
            ].contains(argument)
        }
        .map { argument in
            switch argument {
            case "-O":
                return "-Onone"
            case "-DNDEBUG=1":
                return "-DDEBUG=1"
            default:
                return argument
            }
        }
}

private func appendToMockLog(_ path: String, lines: [String]) {
    guard let data = lines.joined().data(using: .utf8) else {
        return
    }
    let fileManager = FileManager.default
    if !fileManager.fileExists(atPath: path) {
        fileManager.createFile(atPath: path, contents: nil)
    }
    guard let handle = FileHandle(forWritingAtPath: path) else {
        return
    }
    defer {
        try? handle.close()
    }
    do {
        try handle.seekToEnd()
        try handle.write(contentsOf: data)
    } catch {
        // Ignore mock log failures; they should not affect real runs.
    }
}

private func writeToStandardOutput(_ message: String) {
    FileHandle.standardOutput.write(Data(message.utf8))
}

private func writeToStandardError(_ message: String) {
    FileHandle.standardError.write(Data(message.utf8))
}
