import Foundation

enum NetworkEndpoints {
    static let registryURL = "http://registry.hyperos.online"
    static let wasmFetchBase = "http://cloud.hyperos.online/api/v1/artifacts"
}

struct WorkerConfigSnapshot: Equatable {
    var name: String
    var region: String
    var wasmCacheDir: String
    var vfkitPath: String
    var kernelPath: String
    var initrdPath: String
    var cpuPercent: Int
    var memoryMb: Int
    var pidsMax: Int

    var registryURL: String { NetworkEndpoints.registryURL }
    var wasmFetchBase: String { NetworkEndpoints.wasmFetchBase }

    static func fresh() -> WorkerConfigSnapshot {
        var snapshot = WorkerConfigSnapshot(
            name: "",
            region: "",
            wasmCacheDir: Paths.identityDirectory.path,
            vfkitPath: "",
            kernelPath: "",
            initrdPath: "",
            cpuPercent: 25,
            memoryMb: 512,
            pidsMax: 128
        )
        BundledRuntime.apply(to: &snapshot)
        return snapshot
    }

    func toml() -> String {
        var out = """
        [worker]
        backend = "vm"
        listen_addr = "/ip4/0.0.0.0/tcp/0"
        registry_url = \(Self.quote(NetworkEndpoints.registryURL))
        wasm_fetch_base = \(Self.quote(NetworkEndpoints.wasmFetchBase))
        wasm_fetch_timeout_secs = 60
        registry_heartbeat_secs = 30
        wasm_cache_dir = \(Self.quote(wasmCacheDir))
        """
        if !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            out += "\nname = \(Self.quote(name))"
        }
        if !region.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            out += "\nregion = \(Self.quote(region))"
        }
        out += """


        [worker.quota]
        cpu_percent = \(cpuPercent)
        memory_mb = \(memoryMb)
        pids_max = \(pidsMax)

        [worker.vm]
        vfkit_path = \(Self.quote(vfkitPath))
        kernel_path = \(Self.quote(kernelPath))
        initrd_path = \(Self.quote(initrdPath))
        """
        return out + "\n"
    }

    static func parse(toml: String) -> WorkerConfigSnapshot {
        var snapshot = WorkerConfigSnapshot.fresh()
        snapshot.name = string(named: "name", in: toml) ?? snapshot.name
        snapshot.region = string(named: "region", in: toml) ?? snapshot.region
        snapshot.wasmCacheDir = Paths.identityDirectory.path
        snapshot.cpuPercent = int(named: "cpu_percent", in: toml) ?? snapshot.cpuPercent
        snapshot.memoryMb = int(named: "memory_mb", in: toml) ?? snapshot.memoryMb
        snapshot.pidsMax = int(named: "pids_max", in: toml) ?? snapshot.pidsMax
        BundledRuntime.apply(to: &snapshot)
        return snapshot
    }

    private static func quote(_ value: String) -> String {
        "\"\(value.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\""))\""
    }

    private static func string(named key: String, in toml: String) -> String? {
        let pattern = #"\#(key)\s*=\s*"([^"]*)""#
        guard let regex = try? NSRegularExpression(pattern: pattern) else {
            return nil
        }
        let range = NSRange(toml.startIndex..<toml.endIndex, in: toml)
        guard let match = regex.firstMatch(in: toml, range: range),
              let valueRange = Range(match.range(at: 1), in: toml)
        else {
            return nil
        }
        let value = String(toml[valueRange])
        return value.isEmpty ? nil : value
    }

    private static func int(named key: String, in toml: String) -> Int? {
        let pattern = #"\#(key)\s*=\s*(\d+)"#
        guard let regex = try? NSRegularExpression(pattern: pattern) else {
            return nil
        }
        let range = NSRange(toml.startIndex..<toml.endIndex, in: toml)
        guard let match = regex.firstMatch(in: toml, range: range),
              let valueRange = Range(match.range(at: 1), in: toml)
        else {
            return nil
        }
        return Int(toml[valueRange])
    }
}

enum BundledRuntime {
    static var vfkitURL: URL? {
        guard let exe = Bundle.main.executableURL else {
            return nil
        }
        let url = exe.deletingLastPathComponent().appendingPathComponent("vfkit")
        return FileManager.default.isExecutableFile(atPath: url.path) ? url : nil
    }

    static var kernelURL: URL? {
        existingResource(name: "Image", ext: nil)
    }

    static var initrdURL: URL? {
        existingResource(name: "initrd", ext: "img")
    }

    static var isComplete: Bool {
        vfkitURL != nil && kernelURL != nil && initrdURL != nil
    }

    static func apply(to snapshot: inout WorkerConfigSnapshot) {
        if let vfkitURL {
            snapshot.vfkitPath = vfkitURL.path
        }
        if let kernelURL {
            snapshot.kernelPath = kernelURL.path
        }
        if let initrdURL {
            snapshot.initrdPath = initrdURL.path
        }
    }

    private static func existingResource(name: String, ext: String?) -> URL? {
        guard let url = Bundle.main.url(forResource: name, withExtension: ext, subdirectory: "vm"),
              FileManager.default.fileExists(atPath: url.path)
        else {
            return nil
        }
        return url
    }
}

enum Paths {
    static var supportDirectory: URL {
        let url = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/Beenet")
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    static var defaultConfigURL: URL {
        supportDirectory.appendingPathComponent("config.toml")
    }

    static var identityDirectory: URL {
        supportDirectory.appendingPathComponent("cache")
    }

    static var hasLocalIdentity: Bool {
        FileManager.default.fileExists(
            atPath: identityDirectory.appendingPathComponent("identity.key").path
        )
    }
}
