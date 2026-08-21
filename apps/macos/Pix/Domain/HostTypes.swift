import Foundation

enum HostStatus: Equatable {
    case starting
    case ready
    case needsSetup(String)
    case failed(String)

    var title: String {
        switch self {
        case .starting: String(localized: "Starting")
        case .ready: String(localized: "Ready")
        case .needsSetup: String(localized: "Setup needed")
        case .failed: String(localized: "Unavailable")
        }
    }

    var detail: String? {
        switch self {
        case .needsSetup(let message), .failed(let message): message
        case .starting, .ready: nil
        }
    }

    var tint: ColorToken {
        switch self {
        case .starting: .warning
        case .ready: .success
        case .needsSetup: .warning
        case .failed: .danger
        }
    }
}

enum ColorToken {
    case success, warning, danger, neutral
}

struct WorkspaceItem: Identifiable, Equatable {
    let id: UUID
    let name: String
    let path: URL
    var isAvailable: Bool
}

struct PairedDevice: Identifiable, Equatable {
    let id: String
    let name: String
    let pairedAt: Date
}

struct ActiveSession: Identifiable, Equatable {
    let id: String
    let workspacePath: String
    let clients: Int
    let state: String

    var workspaceName: String {
        URL(fileURLWithPath: workspacePath).lastPathComponent
    }

    var isRunning: Bool {
        state == "running"
    }
}

struct PairingRequest: Identifiable, Equatable {
    let id: UUID
    let deviceName: String
    let confirmationCode: String
    let expiresAt: Date
}

/// One short-lived remote pairing offer to render as a QR code.
///
/// The payload embeds the relay endpoint, the single-use pairing channel
/// secret, and the host fingerprint. It never contains workspace, session,
/// or conversation data, and it expires two minutes after creation.
struct RemotePairingOffer: Equatable {
    let qrPayload: String
    let joinCode: String
    let expiresAt: Date
}

enum HostTextInventory {
    static func devices(from output: String) -> [PairedDevice] {
        var result: [PairedDevice] = []
        var pending: (String, String)?
        for line in output.split(separator: "\n", omittingEmptySubsequences: true) {
            if !line.hasPrefix(" "), let separator = line.firstIndex(of: " ") {
                let id = String(line[..<separator])
                let name = line[line.index(after: separator)...].trimmingCharacters(in: .whitespaces)
                if id != "No" {
                    pending = (id, name)
                }
            } else if let current = pending {
                let stamp = String(line).trimmingCharacters(in: .whitespaces)
                    .replacingOccurrences(of: "paired ", with: "")
                result.append(
                    PairedDevice(
                        id: current.0,
                        name: current.1,
                        pairedAt: ISO8601DateFormatter().date(from: stamp) ?? .distantPast
                    )
                )
                pending = nil
            }
        }
        return result
    }
}
