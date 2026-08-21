import SwiftUI

struct HostMenuView: View {
    @Environment(HostModel.self) private var model
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Pix")
                    .font(.headline)
                HStack(spacing: 6) {
                    Circle()
                        .fill(model.status.tint.color)
                        .frame(width: 8, height: 8)
                    Text(model.status.title)
                        .foregroundStyle(.secondary)
                }
                if let detail = model.status.detail ?? model.lastDiagnostic {
                    Text(detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)

            Divider()

            Button {
                model.addWorkspace()
            } label: {
                Label(String(localized: "Add Workspace…"), systemImage: "folder.badge.plus")
            }

            if !model.workspaces.isEmpty {
                Menu {
                    ForEach(model.workspaces) { workspace in
                        Button {
                            // Session navigation belongs to iOS; the desktop
                            // surface only manages authorization in v1.
                        } label: {
                            Label(workspace.name, systemImage: workspace.isAvailable ? "folder" : "folder.badge.questionmark")
                        }
                    }
                } label: {
                    Label(String(localized: "Workspaces"), systemImage: "square.grid.2x2")
                }
            }

            if model.devices.isEmpty {
                Text(String(localized: "No paired devices"))
                    .foregroundStyle(.secondary)
            } else {
                Menu {
                    ForEach(model.devices) { device in
                        Button(role: .destructive) {
                            model.revoke(device)
                        } label: {
                            Label(String(localized: "Revoke \(device.name)"), systemImage: "iphone.slash")
                        }
                    }
                } label: {
                    Label(String(localized: "Paired Devices"), systemImage: "iphone.gen3")
                }
            }

            if model.sessions.isEmpty {
                Text(String(localized: "No active sessions"))
                    .foregroundStyle(.secondary)
            } else {
                Menu {
                    ForEach(model.sessions) { session in
                        Button(role: .destructive) {
                            model.release(session)
                        } label: {
                            Label(
                                String(localized: "Release \(session.workspaceName)"),
                                systemImage: session.isRunning ? "stop.circle" : "moon.zzz"
                            )
                        }
                    }
                } label: {
                    Label(String(localized: "Active Sessions"), systemImage: "bolt.circle")
                }
            }

            if model.pairingRequests.isEmpty {
                Text(String(localized: "No pairing requests"))
                    .foregroundStyle(.secondary)
            } else {
                // Menu-style extras only render Buttons/Toggles reliably.
                // A nested Menu wrapping a VStack silently dropped requests.
                ForEach(model.pairingRequests) { request in
                    Button {
                        model.approve(request)
                    } label: {
                        Text(String(localized: "Approve \(request.deviceName) · \(request.confirmationCode)"))
                    }
                    Button(role: .destructive) {
                        model.reject(request)
                    } label: {
                        Text(String(localized: "Reject \(request.deviceName)"))
                    }
                }
            }

            if model.relayURL != nil {
                Button {
                    // The window's own task requests the pairing channel;
                    // requesting here too would race it and flash a QR code
                    // for a channel that is about to be replaced.
                    openWindow(id: "remote-pairing")
                    NSApp.activate(ignoringOtherApps: true)
                } label: {
                    Label(String(localized: "Pair iPhone Remotely…"), systemImage: "qrcode")
                }
            }

            Divider()

            Button {
                model.setLaunchAtLogin(!model.launchAtLoginEnabled)
            } label: {
                Label(
                    model.launchAtLoginEnabled
                        ? String(localized: "Launch at Login On")
                        : String(localized: "Launch at Login Off"),
                    systemImage: model.launchAtLoginEnabled ? "checkmark.circle" : "circle"
                )
            }

            Button {
                model.refresh()
            } label: {
                Label(String(localized: "Refresh Diagnostics"), systemImage: "arrow.clockwise")
            }
            SettingsLink {
                Label(String(localized: "Settings…"), systemImage: "gearshape")
            }
            Divider()
            Button(String(localized: "Quit Pix")) {
                NSApplication.shared.terminate(nil)
            }
        }
        .padding(.vertical, 4)
        .frame(width: 280)
        .task {
            model.start()
        }
    }
}

struct StatusLabel: View {
    let status: HostStatus

    var body: some View {
        Image(systemName: status == .ready ? "antenna.radiowaves.left.and.right" : "antenna.radiowaves.left.and.right.slash")
            .accessibilityLabel(status.title)
    }
}

extension ColorToken {
    var color: Color {
        switch self {
        case .success: .green
        case .warning: .orange
        case .danger: .red
        case .neutral: .secondary
        }
    }
}
