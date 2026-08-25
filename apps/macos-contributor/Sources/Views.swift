import AppKit
import SwiftUI

struct MainView: View {
    @EnvironmentObject var model: ContributorModel

    var body: some View {
        Group {
            if model.page == .contribute {
                ContributePage()
            } else {
                ActivityPage()
            }
        }
        .padding(.horizontal, 32)
        .padding(.top, 12)
        .padding(.bottom, 24)
        .frame(width: 400, height: 580)
        .background(Color(nsColor: .windowBackgroundColor))
        .toolbar {
            ToolbarItem(placement: .principal) {
                Picker("页面", selection: $model.page) {
                    Text("贡献").tag(ContributorModel.Page.contribute)
                    Text("运行").tag(ContributorModel.Page.activity)
                }
                .pickerStyle(.segmented)
                .frame(width: 132)
                .labelsHidden()
            }
        }
        .onAppear {
            WindowChrome.apply()
            if !model.hasCloudSession {
                model.message = "请先登录 Cloud 平台"
            } else if !model.hasIdentity {
                model.message = "点「开始贡献」，会向 Cloud 申请入网"
            }
            model.refreshCloudPoints()
        }
        .sheet(isPresented: $model.showSettings) {
            SettingsView()
                .environmentObject(model)
        }
        .environmentObject(model)
    }
}

struct ContributePage: View {
    @EnvironmentObject var model: ContributorModel

    var body: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 8)
            VStack(spacing: 14) {
                AppMark(active: model.status.running)
                Text(model.status.running ? "贡献中" : "已暂停")
                    .font(.system(size: 34, weight: .regular, design: .serif))
                    .tracking(1)
                Text(statusLine)
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity)
            Spacer(minLength: 20)
            quota
            if let message = model.displayMessage {
                Text(message)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.top, 16)
            }
            if let url = model.cloudLoginURL {
                Link("打不开浏览器的话，点这里打开登录页", destination: url)
                    .font(.system(size: 12, weight: .medium))
                    .padding(.top, 8)
            }
            Spacer(minLength: 24)
            Button {
                if !model.hasCloudSession {
                    model.loginCloud()
                } else if model.status.running {
                    model.stop()
                } else {
                    model.start()
                }
            } label: {
                Text(primaryButtonTitle)
                    .font(.system(size: 15, weight: .semibold))
                    .frame(maxWidth: .infinity)
                    .frame(height: 46)
            }
            .buttonStyle(HoneyButtonStyle(emphasized: !model.status.running || !model.hasCloudSession))
            .disabled(model.busy)
            HStack(spacing: 20) {
                Button("设置") { model.openSettings() }
                Button("日志") { model.revealLogs() }
                Spacer()
            }
            .buttonStyle(.plain)
            .font(.system(size: 12, weight: .medium))
            .foregroundStyle(.secondary)
            .padding(.top, 16)
        }
    }

    private var primaryButtonTitle: String {
        if !model.hasCloudSession {
            if !model.busy {
                return "登录 Cloud 平台"
            }
            return model.cloudUserCode == nil ? "正在连接 Cloud…" : "等待浏览器登录…"
        }
        if model.busy {
            return "正在切换…"
        }
        return model.status.running ? "停止贡献" : "开始贡献"
    }

    private var quota: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                Text("算力")
                    .font(.system(size: 11, weight: .semibold))
                    .tracking(1.4)
                    .foregroundStyle(.secondary)
                Spacer()
                Text(model.envelopeLabel)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
            VStack(spacing: 0) {
                ForEach(Quota.presets) { preset in
                    Button {
                        model.applyPreset(preset)
                    } label: {
                        QuotaRow(preset: preset, selected: model.selectedPreset == preset)
                    }
                    .buttonStyle(.plain)
                    .disabled(model.busy)
                    if preset.id != Quota.presets.last?.id {
                        Divider().opacity(0.25).padding(.leading, 18)
                    }
                }
            }
            .background(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .fill(Color.primary.opacity(0.04))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .strokeBorder(Color.primary.opacity(0.06), lineWidth: 1)
            )
        }
    }

    private var statusLine: String {
        var parts: [String] = []
        if !model.snapshot.name.isEmpty {
            parts.append(model.snapshot.name)
        }
        if !model.snapshot.region.isEmpty {
            parts.append(model.snapshot.region)
        }
        if !model.hasCloudSession {
            parts.append("未登录 Cloud")
        } else if !model.hasIdentity {
            parts.append("尚未入网")
        }
        if parts.isEmpty {
            return model.hasCloudSession ? "已登录 Cloud" : "在设置里登录 Cloud，再开始贡献"
        }
        return parts.joined(separator: "  ·  ")
    }
}

struct ActivityPage: View {
    @EnvironmentObject var model: ContributorModel

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            if model.status.running, let sample = model.liveSample {
                UsageMeter(
                    title: "CPU",
                    value: String(format: "%.0f%%", sample.cpuPercent),
                    fraction: min(sample.cpuPercent / max(Double(model.snapshot.cpuPercent), 1), 1),
                    caption: "配额 \(model.snapshot.cpuPercent)%",
                    history: model.cpuHistory,
                    scale: max(Double(model.snapshot.cpuPercent), 1)
                )
                UsageMeter(
                    title: "内存",
                    value: String(format: "%.0f MB", sample.memoryMb),
                    fraction: min(sample.memoryMb / max(Double(model.snapshot.memoryMb), 1), 1),
                    caption: "配额 \(model.snapshot.memoryMb) MB · 信封 \(model.envelopeLabel)",
                    history: model.memoryHistory,
                    scale: max(Double(model.snapshot.memoryMb), 1)
                )
            } else {
                VStack(spacing: 10) {
                    Text("开始贡献后显示占用")
                        .font(.system(size: 16, weight: .medium, design: .serif))
                    Text("这里看的是本机 vfkit 进程的 CPU 和内存，以及最近执行过的 Wasm 任务。")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
                .frame(maxWidth: .infinity, minHeight: 140)
            }

            VStack(alignment: .leading, spacing: 10) {
                Text("最近任务")
                    .font(.system(size: 11, weight: .semibold))
                    .tracking(1.4)
                    .foregroundStyle(.secondary)
                if model.recentInvokes.isEmpty {
                    Text("还没有收到调用")
                        .font(.system(size: 13))
                        .foregroundStyle(.secondary)
                        .padding(.vertical, 8)
                } else {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(model.recentInvokes.prefix(8)) { event in
                            HStack {
                                Text(shortCid(event.cid))
                                    .font(.system(size: 12, design: .monospaced))
                                Spacer()
                            }
                        }
                    }
                }
            }
            Spacer()
        }
        .padding(.top, 12)
    }

    private func shortCid(_ cid: String) -> String {
        if cid.count <= 22 {
            return cid
        }
        return "\(cid.prefix(10))…\(cid.suffix(8))"
    }
}

struct UsageMeter: View {
    let title: String
    let value: String
    let fraction: Double
    let caption: String
    let history: [Double]
    let scale: Double

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                Text(title)
                    .font(.system(size: 11, weight: .semibold))
                    .tracking(1.4)
                    .foregroundStyle(.secondary)
                Spacer()
                Text(value)
                    .font(.system(size: 20, weight: .medium, design: .serif))
            }
            GeometryReader { geo in
                ZStack(alignment: .leading) {
                    Capsule().fill(Color.primary.opacity(0.08))
                    Capsule()
                        .fill(BeenetPalette.honey)
                        .frame(width: max(6, geo.size.width * fraction))
                }
            }
            .frame(height: 6)
            Sparkline(values: history, scale: scale)
                .frame(height: 36)
            Text(caption)
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
        }
    }
}

struct Sparkline: View {
    let values: [Double]
    let scale: Double

    var body: some View {
        Canvas { context, size in
            guard values.count > 1, scale > 0 else { return }
            var path = Path()
            for (index, value) in values.enumerated() {
                let x = size.width * CGFloat(index) / CGFloat(values.count - 1)
                let y = size.height * (1 - CGFloat(min(value / scale, 1)))
                if index == 0 {
                    path.move(to: CGPoint(x: x, y: y))
                } else {
                    path.addLine(to: CGPoint(x: x, y: y))
                }
            }
            context.stroke(path, with: .color(BeenetPalette.honey.opacity(0.85)), lineWidth: 1.2)
        }
    }
}

struct QuotaRow: View {
    let preset: QuotaPreset
    let selected: Bool

    var body: some View {
        HStack(spacing: 12) {
            Capsule()
                .fill(selected ? BeenetPalette.honey : Color.clear)
                .frame(width: 3, height: 22)
            Text(preset.label)
                .font(.system(size: 14, weight: selected ? .semibold : .regular))
            Spacer()
            Text("\(preset.cpuPercent)%")
                .font(.system(size: 12, design: .monospaced))
                .foregroundStyle(.secondary)
            Text(memoryLabel)
                .font(.system(size: 12, design: .monospaced))
                .foregroundStyle(.secondary)
                .frame(width: 64, alignment: .trailing)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 11)
        .background(selected ? BeenetPalette.honeySoft : Color.clear)
        .contentShape(Rectangle())
    }

    private var memoryLabel: String {
        if preset.memoryMb >= 1024 {
            let gb = Double(preset.memoryMb) / 1024
            return gb == Double(Int(gb)) ? "\(Int(gb)) GB" : String(format: "%.1f GB", gb)
        }
        return "\(preset.memoryMb) MB"
    }
}

struct HoneyButtonStyle: ButtonStyle {
    let emphasized: Bool

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(emphasized ? Color.white : BeenetPalette.honey)
            .background(
                RoundedRectangle(cornerRadius: 13, style: .continuous)
                    .fill(emphasized ? BeenetPalette.honey : BeenetPalette.honeySoft)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 13, style: .continuous)
                    .strokeBorder(BeenetPalette.honey.opacity(emphasized ? 0 : 0.35), lineWidth: 1)
            )
            .opacity(configuration.isPressed ? 0.82 : 1)
    }
}

enum WindowChrome {
    static func apply() {
        for window in NSApp.windows where window.styleMask.contains(.titled) {
            window.isMovableByWindowBackground = true
            window.titlebarAppearsTransparent = false
            window.titleVisibility = .visible
            window.isOpaque = true
            window.backgroundColor = .windowBackgroundColor
        }
    }
}

struct MenuBarView: View {
    @EnvironmentObject var model: ContributorModel

    var body: some View {
        Button(model.status.running ? "停止贡献" : (model.hasCloudSession ? "开始贡献" : "登录 Cloud 平台")) {
            if !model.hasCloudSession {
                model.loginCloud()
            } else if model.status.running {
                model.stop()
            } else {
                model.start()
            }
        }
        .disabled(model.busy)
        Button("打开窗口") {
            NSApp.activate(ignoringOtherApps: true)
            WindowChrome.apply()
            for window in NSApp.windows {
                window.makeKeyAndOrderFront(nil)
            }
        }
        Button("运行情况") {
            model.page = .activity
            NSApp.activate(ignoringOtherApps: true)
            WindowChrome.apply()
            for window in NSApp.windows {
                window.makeKeyAndOrderFront(nil)
            }
        }
        Divider()
        Button("设置…") { model.openSettings() }
        Divider()
        Button("退出 Beenet") {
            NSApplication.shared.terminate(nil)
        }
    }
}

struct SettingsView: View {
    @EnvironmentObject var model: ContributorModel

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("设置")
                    .font(.headline)
                Spacer()
                Button("完成") { model.showSettings = false }
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 14)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Cloud")
                            .font(.system(size: 11, weight: .semibold))
                            .tracking(1.4)
                            .foregroundStyle(.secondary)
                        if model.hasCloudSession {
                            labeled("账号") {
                                Text(model.cloudEmail)
                                    .font(.caption)
                                    .textSelection(.enabled)
                            }
                            if let points = model.cloudPoints {
                                labeled("积分") {
                                    Text("\(points)")
                                        .font(.system(size: 20, weight: .medium, design: .serif))
                                }
                            }
                            HStack {
                                Button("刷新积分") { model.refreshCloudPoints() }
                                Button("退出登录") { model.logoutCloud() }
                            }
                        } else {
                            Text(settingsLoginHint)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            Button(loginButtonTitle) {
                                model.loginCloud()
                            }
                            .buttonStyle(.borderedProminent)
                            .disabled(model.busy)
                            if let url = model.cloudLoginURL {
                                Link("打开登录页 \(url.absoluteString)", destination: url)
                                    .font(.caption)
                            }
                        }
                    }
                    VStack(alignment: .leading, spacing: 10) {
                        Text("节点")
                            .font(.system(size: 11, weight: .semibold))
                            .tracking(1.4)
                            .foregroundStyle(.secondary)
                        labeled("名称") {
                            TextField("worker-hk-host-1", text: $model.snapshot.name)
                                .textFieldStyle(.roundedBorder)
                        }
                        labeled("地区") {
                            TextField("cn-hongkong", text: $model.snapshot.region)
                                .textFieldStyle(.roundedBorder)
                        }
                        labeled("入网") {
                            Text(identityCaption)
                                .font(.caption)
                                .foregroundStyle(model.hasIdentity ? .secondary : Color.primary)
                        }
                    }
                    VStack(alignment: .leading, spacing: 10) {
                        Text("网络")
                            .font(.system(size: 11, weight: .semibold))
                            .tracking(1.4)
                            .foregroundStyle(.secondary)
                        labeled("Registry") {
                            Text(NetworkEndpoints.registryURL)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .textSelection(.enabled)
                        }
                        labeled("产物") {
                            Text(NetworkEndpoints.wasmFetchBase)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .textSelection(.enabled)
                        }
                        Text("这些地址由 Beenet 指定，不能在 App 里修改。")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                .padding(20)
            }
        }
        .frame(width: 480, height: 380)
        .onChange(of: model.snapshot.name) { _ in model.saveConfig() }
        .onChange(of: model.snapshot.region) { _ in model.saveConfig() }
    }

    private func labeled<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
            content()
        }
    }

    private var settingsLoginHint: String {
        if let message = model.displayMessage, !message.isEmpty {
            return message
        }
        if let code = model.cloudUserCode {
            return "已打开浏览器，配对码 \(code)"
        }
        return "登录后才能申请入网并开始贡献。"
    }

    private var loginButtonTitle: String {
        if !model.busy {
            return "登录 Cloud 平台"
        }
        return model.cloudUserCode == nil ? "正在连接 Cloud…" : "等待浏览器登录…"
    }

    private var identityCaption: String {
        if !model.hasCloudSession {
            return "登录 Cloud 后，开始贡献时会自动申请入网。"
        }
        if model.hasIdentity {
            return "本机节点已入网，密钥由 Cloud 签发后保存在本机。"
        }
        return "尚未入网。开始贡献时会向 Cloud 申请凭证并完成本机登记。"
    }
}
