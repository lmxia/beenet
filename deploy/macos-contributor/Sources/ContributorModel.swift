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
    @Published var cloudSessionToken: String
    @Published var cloudEmail: String
    @Published var cloudUserCode: String?
    @Published var cloudLoginURL: URL?
    @Published var cloudPoints: Int?

    private var refreshTask: Task<Void, Never>? = nil
    private var loginTask: Task<Void, Never>? = nil

    init() {
        let defaults = UserDefaults.standard
        var configURL = defaults.string(forKey: "configPath").map(URL.init(fileURLWithPath:))
            ?? Paths.defaultConfigURL
        var snapshotValue: WorkerConfigSnapshot
        if FileManager.default.fileExists(atPath: configURL.path),
           let text = try? String(contentsOf: configURL, encoding: .utf8)
        {
            snapshotValue = WorkerConfigSnapshot.parse(toml: text)
        } else {
            snapshotValue = WorkerConfigSnapshot.fresh()
            configURL = Paths.defaultConfigURL
            BundledRuntime.apply(to: &snapshotValue)
            try? snapshotValue.toml().write(to: configURL, atomically: true, encoding: .utf8)
            defaults.set(configURL.path, forKey: "configPath")
        }
        BundledRuntime.apply(to: &snapshotValue)
        snapshotValue.wasmCacheDir = Paths.identityDirectory.path
        snapshot = snapshotValue
        configPath = configURL.path
        workingDirectory = Paths.supportDirectory.path
        workerPath = WorkerProcess.locateBinary()?.path ?? ""
        cloudSessionToken = defaults.string(forKey: "cloudSessionToken") ?? ""
        cloudEmail = defaults.string(forKey: "cloudEmail") ?? ""
        saveConfig()
        refreshTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refreshStatus()
                self?.refreshLiveMetrics()
                try? await Task.sleep(nanoseconds: 1_000_000_000)
            }
        }
    }

    var isContributing: Bool {
        status.running && status.heartbeat
    }

    var menuTitle: String {
        if isContributing {
            return "Beenet · 贡献中"
        }
        if status.running {
            return "Beenet · 未在线"
        }
        return "Beenet · 已暂停"
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
        Paths.hasLocalIdentity
    }

    var hasCloudSession: Bool {
        !cloudSessionToken.isEmpty
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
        snapshot.wasmCacheDir = Paths.identityDirectory.path
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
        guard hasCloudSession else {
            message = "请先登录 Cloud 平台"
            openSettings()
            return
        }
        if hasIdentity {
            runWorker("start") { [weak self] in
                guard let self else { return }
                if await self.waitUntilRunning() {
                    self.message = "已开始贡献"
                } else {
                    self.message = self.startFailureMessage()
                }
            }
            return
        }
        enrollThenStart()
    }

    func loginCloud() {
        loginTask?.cancel()
        busy = true
        cloudLoginURL = nil
        message = "正在连接 Cloud…"
        LoginLog.write("loginCloud tapped")
        loginTask = Task { [weak self] in
            guard let self else { return }
            do {
                let started = try await CloudClient.startDeviceLogin()
                try Task.checkCancellation()
                self.cloudUserCode = started.user_code
                guard let url = URL(string: started.verification_uri) else {
                    throw CloudError.badURL
                }
                self.cloudLoginURL = url
                let opened = Browser.open(url)
                LoginLog.write("browser open \(opened) \(url.absoluteString)")
                self.message = opened
                    ? "已打开浏览器，登录后回到 App。配对码 \(started.user_code)"
                    : "请点击下面的链接完成登录。配对码 \(started.user_code)"
                let deadline = Date().addingTimeInterval(TimeInterval(started.expires_in))
                while Date() < deadline {
                    try Task.checkCancellation()
                    let polled = try await CloudClient.pollDeviceLogin(deviceCode: started.device_code)
                    if polled.status == "approved", let token = polled.token, let user = polled.user {
                        self.persistCloudSession(token: token, email: user.email)
                        self.cloudUserCode = nil
                        self.cloudLoginURL = nil
                        self.message = "已登录 \(user.email)"
                        self.refreshCloudPoints()
                        self.busy = false
                        LoginLog.write("login approved \(user.email)")
                        return
                    }
                    try await Task.sleep(nanoseconds: 2_000_000_000)
                }
                self.cloudUserCode = nil
                self.message = "登录超时，请再点一次登录"
            } catch is CancellationError {
                self.cloudUserCode = nil
                LoginLog.write("login cancelled")
            } catch {
                self.cloudUserCode = nil
                self.message = error.localizedDescription
                LoginLog.write("login failed: \(error.localizedDescription)")
            }
            self.busy = false
        }
    }

    func logoutCloud() {
        loginTask?.cancel()
        persistCloudSession(token: "", email: "")
        cloudUserCode = nil
        cloudLoginURL = nil
        cloudPoints = nil
        message = "已退出 Cloud"
    }

    func refreshCloudPoints() {
        let token = cloudSessionToken
        guard !token.isEmpty else { return }
        Task { [weak self] in
            self?.cloudPoints = try? await CloudClient.points(session: token)
        }
    }

    private func persistCloudSession(token: String, email: String) {
        cloudSessionToken = token
        cloudEmail = email
        UserDefaults.standard.set(token, forKey: "cloudSessionToken")
        UserDefaults.standard.set(email, forKey: "cloudEmail")
    }

    private func enrollThenStart() {
        guard let binary = resolvedBinary() else {
            message = WorkerError.missingBinary.errorDescription
            return
        }
        busy = true
        message = "正在向 Cloud 申请入网凭证…"
        let session = cloudSessionToken
        let config = URL(fileURLWithPath: configPath)
        let cwd = URL(fileURLWithPath: workingDirectory)
        let name = snapshot.name
        let region = snapshot.region
        Task { [weak self] in
            guard let self else { return }
            do {
                let minted = try await CloudClient.mintBootstrapToken(session: session)
                let output = try await Task.detached(priority: .userInitiated) {
                    try WorkerProcess.run(
                        binary: binary,
                        config: config,
                        workingDirectory: cwd,
                        command: "enroll",
                        extraArguments: ["--join-token-stdin"],
                        stdin: minted.token_value + "\n"
                    )
                }.value
                guard let peerId = WorkerProcess.parseEnroll(output) else {
                    throw CloudError.decode(output.isEmpty ? "入网成功但没有返回 peer_id" : output)
                }
                try await CloudClient.claimWorker(
                    session: session,
                    peerId: peerId,
                    name: name,
                    region: region,
                    tokenId: minted.id
                )
                self.message = "入网完成，正在开始贡献"
                self.runWorker("start") { [weak self] in
                    guard let self else { return }
                    if await self.waitUntilRunning() {
                        self.message = "已开始贡献"
                    } else {
                        self.message = self.startFailureMessage()
                    }
                }
            } catch {
                self.busy = false
                self.message = error.localizedDescription
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

    func syncNameRegionToCloud() {
        let session = cloudSessionToken
        guard !session.isEmpty else { return }
        let peerId = status.peerId?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard !peerId.isEmpty else { return }
        let name = snapshot.name
        let region = snapshot.region
        Task { [weak self] in
            do {
                try await CloudClient.claimWorker(
                    session: session,
                    peerId: peerId,
                    name: name,
                    region: region
                )
                self?.message = "名称和地区已同步到 Cloud"
            } catch {
                self?.message = error.localizedDescription
            }
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
        guard let handle = try? FileHandle(forReadingFrom: log) else {
            return ""
        }
        defer { try? handle.close() }
        let size = (try? handle.seekToEnd()) ?? 0
        let window = min(size, 16 * 1024)
        try? handle.seek(toOffset: size - window)
        let text = String(data: handle.readDataToEndOfFile(), encoding: .utf8) ?? ""
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
