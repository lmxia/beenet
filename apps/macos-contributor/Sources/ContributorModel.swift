import AppKit
import Combine
import Foundation

@MainActor
final class ContributorModel: ObservableObject {
    enum Page: String {
        case contribute
        case activity
    }

    @Published var page: Page = .contribute
    @Published var snapshot: WorkerConfigSnapshot
    @Published var status = WorkerStatus()
    @Published var busy = false
    @Published var message: String?
    @Published var workerPath: String
    @Published var configPath: String
    @Published var workingDirectory: String
    @Published var liveSample: ProcessSample?
    @Published var cpuHistory: [Double] = []
    @Published var memoryHistory: [Double] = []
    @Published var recentInvokes: [InvokeEvent] = []
    @Published var showSettings = false

    private var refreshTask: Task<Void, Never>? = nil

    init() {
        let defaults = UserDefaults.standard
        let support = Paths.supportDirectory
        var configURL = defaults.string(forKey: "configPath").map(URL.init(fileURLWithPath:))
            ?? Paths.defaultConfigURL
        var snapshotValue: WorkerConfigSnapshot
        if FileManager.default.fileExists(atPath: configURL.path),
           let text = try? String(contentsOf: configURL, encoding: .utf8)
        {
            snapshotValue = WorkerConfigSnapshot.parse(toml: text)
        } else {
            snapshotValue = WorkerConfigSnapshot.fresh(supportDir: support)
            configURL = Paths.defaultConfigURL
            BundledRuntime.apply(to: &snapshotValue)
            try? snapshotValue.toml().write(to: configURL, atomically: true, encoding: .utf8)
            defaults.set(configURL.path, forKey: "configPath")
        }
        BundledRuntime.apply(to: &snapshotValue)
        snapshot = snapshotValue
        configPath = configURL.path
        workingDirectory = Paths.supportDirectory.path
        workerPath = WorkerProcess.locateBinary()?.path ?? ""
        saveConfig()
        refreshTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refreshStatus()
                self?.refreshLiveMetrics()
                try? await Task.sleep(nanoseconds: 1_000_000_000)
            }
        }
    }

    var menuTitle: String {
        status.running ? "Beenet · 贡献中" : "Beenet · 已暂停"
    }

    var displayMessage: String? {
        guard let message, !message.isEmpty else {
            return nil
        }
        return message
    }

    var envelopeLabel: String {
        let envelope = Quota.envelope(cpuPercent: snapshot.cpuPercent, memoryMb: snapshot.memoryMb)
        return "\(envelope.cpus) vCPU · \(envelope.memoryMb) MB"
    }

    var selectedPreset: QuotaPreset? {
        Quota.matchingPreset(cpuPercent: snapshot.cpuPercent, memoryMb: snapshot.memoryMb)
    }

    var hasIdentity: Bool {
        Paths.isIdentityDirectory(URL(fileURLWithPath: snapshot.wasmCacheDir))
    }

    func applyPreset(_ preset: QuotaPreset) {
        snapshot.cpuPercent = preset.cpuPercent
        snapshot.memoryMb = preset.memoryMb
        snapshot.pidsMax = preset.pidsMax
        saveConfig()
        message = "已改为「\(preset.label)」。停止后再开始贡献才会换配额。"
    }

    func saveConfig() {
        BundledRuntime.apply(to: &snapshot)
        let url = URL(fileURLWithPath: configPath)
        try? FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try? snapshot.toml().write(to: url, atomically: true, encoding: .utf8)
        UserDefaults.standard.set(configPath, forKey: "configPath")
        UserDefaults.standard.set(workingDirectory, forKey: "workingDirectory")
        UserDefaults.standard.set(workerPath, forKey: "workerPath")
    }

    func start() {
        saveConfig()
        guard hasIdentity else {
            message = "请先选择包含 identity.key 的身份目录"
            openSettings()
            return
        }
        runWorker("start") { [weak self] in
            guard let self else { return }
            if await self.waitUntilRunning() {
                self.message = "已开始贡献"
            } else {
                self.message = self.startFailureMessage()
            }
        }
    }

    func stop() {
        runWorker("stop") { [weak self] in
            await self?.refreshStatus()
            self?.message = "已停止贡献"
        }
    }

    func refreshStatus() async {
        guard let binary = resolvedBinary() else {
            return
        }
        do {
            let output = try WorkerProcess.run(
                binary: binary,
                config: URL(fileURLWithPath: configPath),
                workingDirectory: URL(fileURLWithPath: workingDirectory),
                command: "status"
            )
            status = WorkerProcess.parseStatus(output)
            refreshLiveMetrics()
        } catch {
            // Keep the last known status; start/stop surfaces errors.
        }
    }

    func refreshLiveMetrics() {
        if status.running, let pid = status.pid, let sample = ProcessMeter.sample(pid: pid) {
            liveSample = sample
            cpuHistory = Array((cpuHistory + [sample.cpuPercent]).suffix(48))
            memoryHistory = Array((memoryHistory + [sample.memoryMb]).suffix(48))
        } else {
            liveSample = nil
        }
        recentInvokes = InvokeLog.recent(in: snapshot.wasmCacheDir)
    }

    func importIdentityDirectory() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.message = "选择已有的 wasm_cache_dir（目录中需包含 identity.key）"
        panel.prompt = "导入"
        guard panel.runModal() == .OK, let url = panel.url else {
            return
        }
        let directory = url.standardizedFileURL
        guard Paths.isIdentityDirectory(directory) else {
            message = "该目录没有 identity.key，不能作为节点身份"
            return
        }
        snapshot.wasmCacheDir = directory.path
        workingDirectory = Paths.supportDirectory.path
        saveConfig()
        message = "已使用身份目录 \(directory.lastPathComponent)"
        Task { await refreshStatus() }
    }

    func openSettings() {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
        for window in NSApp.windows where window.styleMask.contains(.titled) {
            window.makeKeyAndOrderFront(nil)
        }
        WindowChrome.apply()
        showSettings = true
    }

    func revealLogs() {
        let log = URL(fileURLWithPath: workingDirectory)
            .appendingPathComponent("logs/beenet-worker.log")
        if FileManager.default.fileExists(atPath: log.path) {
            NSWorkspace.shared.activateFileViewerSelecting([log])
        } else {
            NSWorkspace.shared.open(URL(fileURLWithPath: workingDirectory))
        }
    }

    private func runWorker(_ command: String, onSuccess: @escaping @MainActor () async -> Void) {
        saveConfig()
        guard let binary = resolvedBinary() else {
            message = WorkerError.missingBinary.errorDescription
            return
        }
        busy = true
        message = nil
        let config = URL(fileURLWithPath: configPath)
        let cwd = URL(fileURLWithPath: workingDirectory)
        Task { [weak self] in
            let result = await Task.detached(priority: .userInitiated) {
                Result(catching: {
                    try WorkerProcess.run(
                        binary: binary,
                        config: config,
                        workingDirectory: cwd,
                        command: command
                    )
                })
            }.value
            guard let self else { return }
            switch result {
            case .success:
                await onSuccess()
                await self.refreshStatus()
            case .failure(let error):
                self.message = error.localizedDescription
            }
            self.busy = false
        }
    }

    private func waitUntilRunning() async -> Bool {
        for _ in 0..<20 {
            await refreshStatus()
            if hasVirtualizationDenial() {
                await stopQuietly()
                return false
            }
            if status.running {
                try? await Task.sleep(nanoseconds: 1_500_000_000)
                await refreshStatus()
                if hasVirtualizationDenial() || !status.running {
                    await stopQuietly()
                    return false
                }
                return true
            }
            try? await Task.sleep(nanoseconds: 400_000_000)
        }
        return status.running
    }

    private func stopQuietly() async {
        guard let binary = resolvedBinary() else { return }
        _ = try? WorkerProcess.run(
            binary: binary,
            config: URL(fileURLWithPath: configPath),
            workingDirectory: URL(fileURLWithPath: workingDirectory),
            command: "stop"
        )
        await refreshStatus()
    }

    private func startFailureMessage() -> String {
        if hasVirtualizationDenial() {
            return "虚拟机没有虚拟化授权，起不来。请用最新编好的 Beenet.app 再试。"
        }
        let log = lastLogExcerpt()
        if log.isEmpty {
            return "启动命令已返回，但 worker 没有在运行。点「日志」查看。"
        }
        return "worker 没有在运行：\n\(log)"
    }

    private func hasVirtualizationDenial() -> Bool {
        lastLogExcerpt().contains("com.apple.security.virtualization")
    }

    private func lastLogExcerpt() -> String {
        let log = URL(fileURLWithPath: workingDirectory)
            .appendingPathComponent("logs/beenet-worker.log")
        guard let text = try? String(contentsOf: log, encoding: .utf8) else {
            return ""
        }
        let lines = text.split(whereSeparator: \.isNewline).suffix(6)
        return lines.map(String.init).joined(separator: "\n")
    }

    private func resolvedBinary() -> URL? {
        if let found = WorkerProcess.locateBinary() {
            workerPath = found.path
            return found
        }
        if !workerPath.isEmpty, FileManager.default.isExecutableFile(atPath: workerPath) {
            return URL(fileURLWithPath: workerPath)
        }
        return nil
    }
}
