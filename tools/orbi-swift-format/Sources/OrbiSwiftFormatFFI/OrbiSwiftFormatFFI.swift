import Foundation
import SwiftFormat

private struct FormatRequest: Decodable {
    enum Mode: String, Decodable {
        case check
        case write
    }

    let workingDirectory: String
    let configurationJson: String?
    let mode: Mode
    let files: [String]
}

private enum FormatToolError: LocalizedError {
    case usage
    case invalidWorkingDirectory(String)

    var errorDescription: String? {
        switch self {
        case .usage:
            return "usage: orbi-swift-format <request.json>"
        case let .invalidWorkingDirectory(path):
            return "failed to change directory to \(path)"
        }
    }
}

public func orbiSwiftFormatMain(arguments: [String] = CommandLine.arguments) -> Int32 {
    do {
        return try OrbiSwiftFormatTool.run(arguments: arguments)
    } catch {
        writeToStandardError("error: \(error.localizedDescription)\n")
        return 1
    }
}

@_cdecl("orbi_swiftformat_run_request")
public func orbi_swiftformat_run_request(requestPath: UnsafePointer<CChar>?) -> Int32 {
    guard let requestPath else {
        writeToStandardError("error: missing request path for orbi-swift-format\n")
        return 1
    }
    return orbiSwiftFormatMain(arguments: ["orbi-swift-format", String(cString: requestPath)])
}

private enum OrbiSwiftFormatTool {
    static func run(arguments: [String]) throws -> Int32 {
        guard arguments.count == 2 else {
            throw FormatToolError.usage
        }

        let requestPath = arguments[1]
        if let status = try maybeHandleMockRequest(at: requestPath) {
            return status
        }

        let request = try decodeRequest(at: requestPath)
        guard FileManager.default.changeCurrentDirectoryPath(request.workingDirectory) else {
            throw FormatToolError.invalidWorkingDirectory(request.workingDirectory)
        }

        let configuration = try loadConfiguration(from: request.configurationJson)
        switch request.mode {
        case .check:
            let findings = try lint(files: request.files, configuration: configuration)
            guard findings.isEmpty else {
                emit(findings: findings)
                return 2
            }
        case .write:
            try format(files: request.files, configuration: configuration)
        }
        return 0
    }

    private static func decodeRequest(at path: String) throws -> FormatRequest {
        let data = try Data(contentsOf: URL(fileURLWithPath: path))
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(FormatRequest.self, from: data)
    }

    private static func loadConfiguration(from json: String?) throws -> Configuration {
        guard let json else {
            return Configuration()
        }
        return try Configuration(data: Data(json.utf8))
    }

    private static func lint(files: [String], configuration: Configuration) throws -> [Finding] {
        var findings = [Finding]()
        for path in files {
            let consumer: (Finding) -> Void = { finding in
                findings.append(finding)
            }
            let linter = SwiftLinter(configuration: configuration, findingConsumer: consumer)
            try linter.lint(contentsOf: URL(fileURLWithPath: path))
        }
        return findings
    }

    private static func format(files: [String], configuration: Configuration) throws {
        let formatter = SwiftFormatter(configuration: configuration)
        for path in files {
            let url = URL(fileURLWithPath: path)
            let original = try String(contentsOf: url, encoding: .utf8)
            var formatted = ""
            try formatter.format(contentsOf: url, to: &formatted)
            guard original != formatted else {
                continue
            }
            try formatted.write(to: url, atomically: true, encoding: .utf8)
        }
    }

    private static func emit(findings: [Finding]) {
        for finding in findings {
            let location = finding.location ?? Finding.Location(file: "<unknown>", line: 1, column: 1)
            writeToStandardOutput(
                "\(location.file):\(location.line):\(location.column): error: [\(finding.category)] \(finding.message)\n"
            )
            for note in finding.notes {
                let noteLocation = note.location ?? location
                writeToStandardOutput(
                    "\(noteLocation.file):\(noteLocation.line):\(noteLocation.column): note: \(note.message)\n"
                )
            }
        }
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
            "orbi-swift-format \(requestPath)\n",
            "orbi-swift-format request:\n",
            requestBody,
            "\n",
        ]
    )
    return 0
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
