import AppKit
import Foundation

enum CloudEndpoints {
    static let apiBase = "http://cloud.hyperos.online/api"
}

struct CloudUser: Decodable {
    var id: String
    var email: String
}

struct CloudDeviceStart: Decodable {
    var device_code: String
    var user_code: String
    var verification_uri: String
    var expires_in: UInt64
}

struct CloudBootstrapToken: Decodable {
    var id: String
    var token_value: String
    var issued_by: String?
}

struct CloudWorker: Decodable {
    var peer_id: String
    var name: String?
    var region: String?
    var status: String
}

enum Browser {
    @discardableResult
    static func open(_ url: URL) -> Bool {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/usr/bin/open")
        proc.arguments = ["-u", url.absoluteString]
        do {
            try proc.run()
            return true
        } catch {
            LoginLog.write("open command failed: \(error.localizedDescription)")
            return NSWorkspace.shared.open(url)
        }
    }
}

enum LoginLog {
    static func write(_ line: String) {
        let url = Paths.supportDirectory.appendingPathComponent("login.log")
        let stamp = ISO8601DateFormatter().string(from: Date())
        let text = "[\(stamp)] \(line)\n"
        guard let data = text.data(using: .utf8) else { return }
        if FileManager.default.fileExists(atPath: url.path),
           let handle = try? FileHandle(forWritingTo: url)
        {
            _ = try? handle.seekToEnd()
            try? handle.write(contentsOf: data)
            try? handle.close()
            return
        }
        try? data.write(to: url)
    }
}

enum CloudClient {
    private static let session: URLSession = {
        let config = URLSessionConfiguration.ephemeral
        config.waitsForConnectivity = false
        config.timeoutIntervalForRequest = 12
        config.timeoutIntervalForResource = 15
        config.allowsCellularAccess = true
        return URLSession(configuration: config)
    }()

    static func startDeviceLogin() async throws -> CloudDeviceStart {
        try await post("/v1/auth/device/start", body: Data("{}".utf8), token: nil)
    }

    static func pollDeviceLogin(deviceCode: String) async throws -> (status: String, token: String?, user: CloudUser?) {
        struct Poll: Decodable {
            var status: String
            var token: String?
            var user: CloudUser?
        }
        let payload = try JSONSerialization.data(withJSONObject: ["device_code": deviceCode])
        let poll: Poll = try await post("/v1/auth/device/poll", body: payload, token: nil)
        return (poll.status, poll.token, poll.user)
    }

    static func mintBootstrapToken(session: String) async throws -> CloudBootstrapToken {
        try await post("/v1/me/bootstrap-tokens", body: Data("{}".utf8), token: session)
    }

    static func claimWorker(
        session: String,
        peerId: String,
        name: String,
        region: String,
        tokenId: String? = nil
    ) async throws {
        var body: [String: Any] = [
            "peer_id": peerId,
        ]
        if !name.isEmpty { body["name"] = name }
        if !region.isEmpty { body["region"] = region }
        if let tokenId, !tokenId.isEmpty { body["bootstrap_token_id"] = tokenId }
        let payload = try JSONSerialization.data(withJSONObject: body)
        struct Claim: Decodable { var peer_id: String }
        let _: Claim = try await post("/v1/me/workers", body: payload, token: session)
    }

    static func points(session: String) async throws -> Int {
        struct Points: Decodable { var points: Int }
        let value: Points = try await get("/v1/me/points", token: session)
        return value.points
    }

    static func me(session: String) async throws -> CloudUser {
        struct Envelope: Decodable { var user: CloudUser }
        let envelope: Envelope = try await get("/v1/me", token: session)
        return envelope.user
    }

    private static func get<T: Decodable>(_ path: String, token: String?) async throws -> T {
        try await send(path, method: "GET", body: nil, token: token)
    }

    private static func post<T: Decodable>(_ path: String, body: Data, token: String?) async throws -> T {
        try await send(path, method: "POST", body: body, token: token)
    }

    private static func send<T: Decodable>(
        _ path: String,
        method: String,
        body: Data?,
        token: String?
    ) async throws -> T {
        guard let url = URL(string: CloudEndpoints.apiBase + path) else {
            throw CloudError.badURL
        }
        LoginLog.write("\(method) \(url.absoluteString)")
        var request = URLRequest(url: url, timeoutInterval: 12)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if body != nil {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = body
        }
        if let token, !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        let (data, response) = try await session.data(for: request)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        LoginLog.write("\(method) \(path) -> \(status)")
        if !(200..<300).contains(status) {
            throw CloudError.http(status, cloudMessage(data))
        }
        do {
            return try JSONDecoder().decode(T.self, from: data)
        } catch {
            throw CloudError.decode(cloudMessage(data))
        }
    }

    private static func cloudMessage(_ data: Data) -> String {
        struct Envelope: Decodable { var error: Body? }
        struct Body: Decodable { var message: String? }
        if let envelope = try? JSONDecoder().decode(Envelope.self, from: data),
           let message = envelope.error?.message,
           !message.isEmpty
        {
            return localizedCloudMessage(message)
        }
        return localizedCloudMessage(String(data: data, encoding: .utf8) ?? "Cloud 请求失败")
    }

    private static func localizedCloudMessage(_ message: String) -> String {
        if message.contains("unused bootstrap token") {
            return "上一枚入网凭证还在有效期内。请再点一次开始贡献。"
        }
        return message
    }
}

enum CloudError: LocalizedError {
    case badURL
    case http(Int, String)
    case decode(String)

    var errorDescription: String? {
        switch self {
        case .badURL:
            return "Cloud 地址无效"
        case .http(_, let message):
            return message
        case .decode(let message):
            return message
        }
    }
}
