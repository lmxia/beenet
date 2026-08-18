import Foundation

struct WorkerStatus: Equatable {
    var running = false
    var joined = false
    var pid: Int?
    var name: String?
    var backend: String?
    var registryURL: String?
    var raw = ""
}

enum WorkerProcess {
    static func locateBinary() -> URL? {
        for candidate in bundledWorkerCandidates() where FileManager.default.isExecutableFile(atPath: candidate.path) {
            return candidate
        }
        if let env = ProcessInfo.processInfo.environment["BEENET_WORKER"], !env.isEmpty {
            let url = URL(fileURLWithPath: env)
            if FileManager.default.isExecutableFile(atPath: url.path) {
                return url
            }
        }
        return nil
    }

    private static func bundledWorkerCandidates() -> [URL] {
        var urls: [URL] = []
        if let exe = Bundle.main.executableURL {
            urls.append(exe.deletingLastPathComponent().appendingPathComponent("beenet-worker"))
        }
        let argv0 = URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL
        urls.append(argv0.deletingLastPathComponent().appendingPathComponent("beenet-worker"))
        let app = argv0.deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        if app.pathExtension == "app" {
            urls.append(app.appendingPathComponent("Contents/MacOS/beenet-worker"))
        }
        return urls
    }

    static func run(
        binary: URL,
        config: URL,
        workingDirectory: URL,
        command: String
    ) throws -> String {
        let process = Process()
        process.executableURL = binary
        process.arguments = ["--config", config.path, command]
        process.currentDirectoryURL = workingDirectory
        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr
        try process.run()
        process.waitUntilExit()
        let out = String(data: stdout.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        let err = String(data: stderr.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        let combined = (out + err).trimmingCharacters(in: .whitespacesAndNewlines)
        if process.terminationStatus != 0 {
            throw WorkerError.commandFailed(command, combined)
        }
        return combined
    }

    static func parseStatus(_ text: String) -> WorkerStatus {
        var status = WorkerStatus(raw: text)
        for line in text.split(whereSeparator: \.isNewline) {
            let parts = line.split(separator: ":", maxSplits: 1).map {
                $0.trimmingCharacters(in: .whitespaces)
            }
            guard parts.count == 2 else { continue }
            switch parts[0] {
            case "running":
                status.running = parts[1] == "true"
            case "joined":
                status.joined = parts[1] == "true"
            case "pid":
                status.pid = Int(parts[1])
            case "name":
                status.name = parts[1]
            case "backend":
                status.backend = parts[1]
            case "registry_url":
                status.registryURL = parts[1]
            default:
                break
            }
        }
        return status
    }
}

enum WorkerError: LocalizedError {
    case missingBinary
    case commandFailed(String, String)

    var errorDescription: String? {
        switch self {
        case .missingBinary:
            return "找不到 beenet-worker。请先 cargo build --release -p beenet-worker，或把二进制放进 App 包。"
        case .commandFailed(let command, let output):
            if output.isEmpty {
                return "beenet-worker \(command) 失败"
            }
            return output
        }
    }
}
