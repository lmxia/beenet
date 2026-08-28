import Foundation

struct ProcessSample: Equatable {
    var cpuPercent: Double
    var memoryMb: Double
}

enum ProcessMeter {
    static func sample(pid: Int) -> ProcessSample? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/ps")
        process.arguments = ["-o", "%cpu=", "-o", "rss=", "-p", String(pid)]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = Pipe()
        do {
            try process.run()
            process.waitUntilExit()
        } catch {
            return nil
        }
        guard process.terminationStatus == 0 else {
            return nil
        }
        let text = String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let parts = text.split(whereSeparator: \.isWhitespace)
        guard parts.count >= 2,
              let cpu = Double(parts[0]),
              let rssKb = Double(parts[1])
        else {
            return nil
        }
        return ProcessSample(cpuPercent: cpu, memoryMb: rssKb / 1024)
    }
}

struct InvokeEvent: Identifiable, Equatable {
    let id: String
    let cid: String
    let time: Date
}

enum InvokeLog {
    static func recent(in cacheDir: String, limit: Int = 12) -> [InvokeEvent] {
        let url = URL(fileURLWithPath: cacheDir).appendingPathComponent("logs/worker.log")
        guard let handle = try? FileHandle(forReadingFrom: url) else {
            return []
        }
        defer { try? handle.close() }
        let size = (try? handle.seekToEnd()) ?? 0
        let window = min(size, 96 * 1024)
        try? handle.seek(toOffset: size - window)
        let data = handle.readDataToEndOfFile()
        guard let text = String(data: data, encoding: .utf8) else {
            return []
        }
        var events: [InvokeEvent] = []
        for line in text.split(whereSeparator: \.isNewline).reversed() {
            guard line.contains(" invoke") || line.contains("cid=") else {
                continue
            }
            let cid = value(named: "cid", in: String(line)) ?? "unknown"
            let requestId = value(named: "request_id", in: String(line)) ?? UUID().uuidString
            events.append(InvokeEvent(id: requestId, cid: cid, time: Date()))
            if events.count >= limit {
                break
            }
        }
        return events
    }

    private static func value(named key: String, in line: String) -> String? {
        let pattern = #"\#(key)=([^\s]+)"#
        guard let regex = try? NSRegularExpression(pattern: pattern) else {
            return nil
        }
        let range = NSRange(line.startIndex..<line.endIndex, in: line)
        guard let match = regex.firstMatch(in: line, range: range),
              let valueRange = Range(match.range(at: 1), in: line)
        else {
            return nil
        }
        return String(line[valueRange]).trimmingCharacters(in: CharacterSet(charactersIn: "\""))
    }
}
