import Foundation

struct QuotaPreset: Identifiable, Equatable {
    let id: String
    let label: String
    let cpuPercent: Int
    let memoryMb: Int
    let pidsMax: Int
}

enum Quota {
    static let headroomMb = 320
    static let presets: [QuotaPreset] = [
        QuotaPreset(id: "light", label: "轻量", cpuPercent: 10, memoryMb: 256, pidsMax: 64),
        QuotaPreset(id: "balanced", label: "均衡", cpuPercent: 25, memoryMb: 512, pidsMax: 128),
        QuotaPreset(id: "more", label: "更多", cpuPercent: 50, memoryMb: 1024, pidsMax: 128),
        QuotaPreset(id: "heavy", label: "高性能", cpuPercent: 150, memoryMb: 2048, pidsMax: 256),
    ]

    static func envelope(cpuPercent: Int, memoryMb: Int) -> (cpus: Int, memoryMb: Int) {
        let cpus = max(1, Int(ceil(Double(cpuPercent) / 100.0)))
        return (cpus, memoryMb + headroomMb)
    }

    static func matchingPreset(cpuPercent: Int, memoryMb: Int) -> QuotaPreset? {
        presets.first { $0.cpuPercent == cpuPercent && $0.memoryMb == memoryMb }
    }
}
