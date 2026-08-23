import AppKit
import SwiftUI

/// The menu bar is the quick host control surface. Inventory details are
/// progressively disclosed through the Workspaces and Devices submenus, while
/// the pairing guide remains a separate window.
///
/// MenuBarExtra content is rebuilt while the menu opens. We render a snapshot
/// for the duration of that presentation instead of observing live inventories
/// in the menu body; Host socket events therefore cannot interrupt pointer
/// tracking and collapse a submenu underneath the cursor.
struct HostMenuView: View {
    @Environment(HostModel.self) private var model
    @Environment(\.openWindow) private var openWindow
    @State private var snapshot = HostMenuSnapshot.empty

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            statusHeader

            Divider()

            if !model.isConfigured {
                Button {
                    presentSetupWindow()
                } label: {
                    Label(String(localized: "Set up Pix…"), systemImage: "wand.and.stars")
                }

                Text(String(localized: "Pix has no host configuration yet."))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                if !snapshot.pairingRequests.isEmpty {
                    Button {
                        presentPairingWindow()
                    } label: {
                        Label(
                            snapshot.pairingRequests.count == 1
                                ? String(localized: "Review pairing request…")
                                : String(localized: "Review pairing requests…"),
                            systemImage: "person.crop.circle.badge.questionmark"
                        )
                    }
                }

                workspacesMenu
                devicesMenu

                if !snapshot.sessions.isEmpty {
                    sessionsMenu
                }
            }

            Divider()

            SettingsLink {
                Label(String(localized: "Settings…"), systemImage: "gearshape")
            }

            Button {
                NSApplication.shared.terminate(nil)
            } label: {
                Label(String(localized: "Quit Pix"), systemImage: "power")
            }
        }
        .padding(.vertical, 4)
        .frame(width: 300)
        .onAppear {
            snapshot = HostMenuSnapshot(model: model)
        }
        .onChange(of: model.inventoryRevision) { _, newRevision in
            guard HostMenuSnapshot.shouldHydrate(forInventoryRevision: newRevision) else { return }
            // HostModel advances this revision only after the initial CLI and
            // service inventory reconciliation has completed. If the menu
            // opened during startup, hydrate the stable menu snapshot now;
            // later socket events remain isolated from pointer tracking.
            snapshot = HostMenuSnapshot(model: model)
        }
    }

    private var statusHeader: some View {
        LiveStatusHeader()
    }

    /// The inventory menus deliberately use a presentation snapshot so host
    /// socket events cannot rebuild a nested menu under the pointer. Lifecycle
    /// status is different: it changes during the first seconds after launch,
    /// so it must remain live or an early menu open would stay on "Starting"
    /// forever even after the host is ready.
    private struct LiveStatusHeader: View {
        @Environment(HostModel.self) private var model

        var body: some View {
            let presentation = HostMenuStatusPresentation(model: model)
            let status = presentation.status

            SettingsLink {
                HStack(spacing: 6) {
                    Text(String(localized: "Pix"))
                    Spacer(minLength: 10)
                    Text(status.title)
                    Image(systemName: status.menuSymbolName)
                        .foregroundStyle(status.tint.color)
                        .symbolRenderingMode(.hierarchical)
                }
                .font(.headline)
                .foregroundStyle(.primary)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .accessibilityElement(children: .combine)
            .accessibilityLabel(
                String(localized: "Pix") + ", " + status.title
            )
            .accessibilityHint(presentation.detail ?? "")
        }
    }

    private var workspacesMenu: some View {
        Menu {
            Button {
                model.addWorkspace()
            } label: {
                Label(String(localized: "Add Workspace…"), systemImage: "folder.badge.plus")
            }

            if snapshot.workspaces.isEmpty {
                Text(String(localized: "No authorized workspaces"))
                    .foregroundStyle(.secondary)
            } else {
                Divider()
                ForEach(snapshot.workspaces) { workspace in
                    workspaceMenu(workspace)
                }
            }
        } label: {
            Label(
                String(localized: "Workspaces") + " (" + String(snapshot.workspaces.count) + ")",
                systemImage: "folder"
            )
        }
    }

    private func workspaceMenu(_ workspace: WorkspaceItem) -> some View {
        let sessions = snapshot.sessions(for: workspace)

        return Menu {
            Text(workspace.path.path)
                .font(.caption)

            if sessions.isEmpty {
                Text(String(localized: "No sessions"))
                    .foregroundStyle(.secondary)
            } else {
                Divider()
                ForEach(sessions) { session in
                    if let activeSession = session.activeSession {
                        Button(role: .destructive) {
                            model.release(activeSession)
                        } label: {
                            Label(
                                session.displayTitle,
                                systemImage: activeSession.isRunning ? "stop.circle" : "moon.zzz"
                            )
                        }
                    } else {
                        Label(session.displayTitle, systemImage: "clock")
                    }
                }
            }

            Divider()
            Button(role: .destructive) {
                model.removeWorkspace(workspace)
            } label: {
                Label(String(localized: "Remove Workspace"), systemImage: "trash")
            }
        } label: {
            Label(
                workspace.name,
                systemImage: workspace.isAvailable ? "folder" : "folder.badge.questionmark"
            )
        }
    }

    private var devicesMenu: some View {
        Menu {
            Button {
                presentPairingWindow()
            } label: {
                Label(String(localized: "Add Device…"), systemImage: "plus.circle")
            }

            if snapshot.devices.isEmpty {
                Text(String(localized: "No paired devices"))
                    .foregroundStyle(.secondary)
            } else {
                Divider()
                ForEach(snapshot.devices) { device in
                    deviceMenu(device)
                }
            }
        } label: {
            Label(
                String(localized: "Paired Devices") + " (" + String(snapshot.devices.count) + ")",
                systemImage: "iphone.gen3"
            )
        }
    }

    private func deviceMenu(_ device: PairedDevice) -> some View {
        Menu {
            Text(device.id)
                .font(.system(.caption, design: .monospaced))
                .textSelection(.enabled)

            Divider()

            Button(role: .destructive) {
                model.revoke(device)
            } label: {
                Label(String(localized: "Revoke ") + device.name, systemImage: "iphone.slash")
            }
        } label: {
            Label(device.name, systemImage: "iphone")
        }
    }

    private var sessionsMenu: some View {
        Menu {
            ForEach(snapshot.sessions) { session in
                Button(role: .destructive) {
                    model.release(session)
                } label: {
                    Label(
                        String(localized: "Release ") + session.workspaceName,
                        systemImage: session.isRunning ? "stop.circle" : "moon.zzz"
                    )
                }
            }
        } label: {
            Label(
                String(localized: "Active Sessions") + " (" + String(snapshot.sessions.count) + ")",
                systemImage: "bolt.circle"
            )
        }
    }

    private func presentPairingWindow() {
        openWindow(id: "add-device")
        NSApp.activate(ignoringOtherApps: true)
    }

    private func presentSetupWindow() {
        openWindow(id: "setup")
        NSApp.activate(ignoringOtherApps: true)
    }
}

/// The status-item label remains live even while the menu itself is closed, so
/// a pairing request can present the approval guide without user interaction.
struct StatusItemLabel: View {
    @Environment(HostModel.self) private var model
    @Environment(\.openWindow) private var openWindow
    @AppStorage("didPresentSetupGuide") private var didPresentSetupGuide = false

    var body: some View {
        StatusLabel(status: model.status)
            .onChange(of: model.isConfigured) { _, configured in
                guard !configured, !didPresentSetupGuide else { return }
                didPresentSetupGuide = true
                openWindow(id: "setup")
                NSApp.activate(ignoringOtherApps: true)
            }
            .onChange(of: model.pairingRequests) { oldRequests, newRequests in
                let oldIDs = Set(oldRequests.map(\.id))
                let newIDs = Set(newRequests.map(\.id))
                guard !newIDs.subtracting(oldIDs).isEmpty else { return }
                presentPairingWindow()
            }
            .task {
                if !model.pairingRequests.isEmpty {
                    presentPairingWindow()
                }
            }
    }

    private func presentPairingWindow() {
        openWindow(id: "add-device")
        NSApp.activate(ignoringOtherApps: true)
        NSApp.requestUserAttention(.criticalRequest)
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

struct HostMenuStatusPresentation: Equatable {
    let status: HostStatus
    let detail: String?

    init(status: HostStatus, detail: String?) {
        self.status = status
        self.detail = detail
    }

    @MainActor
    init(model: HostModel) {
        self.init(status: model.status, detail: model.status.detail ?? model.lastDiagnostic)
    }
}

struct HostMenuSnapshot {
    let workspaces: [WorkspaceItem]
    let devices: [PairedDevice]
    let sessions: [ActiveSession]
    let workspaceSessions: [WorkspaceSession]
    let pairingRequests: [PairingRequest]

    static let empty = HostMenuSnapshot(
        workspaces: [],
        devices: [],
        sessions: [],
        workspaceSessions: [],
        pairingRequests: []
    )

    static func shouldHydrate(forInventoryRevision revision: Int) -> Bool {
        revision > 0
    }

    @MainActor
    init(model: HostModel) {
        workspaces = model.workspaces
        devices = model.devices
        sessions = model.sessions
        workspaceSessions = model.workspaceSessions
        pairingRequests = model.pairingRequests
    }

    func sessions(for workspace: WorkspaceItem) -> [WorkspaceSession] {
        var result = workspaceSessions.filter { $0.workspaceID == workspace.id }
        let activeSessions = sessions.filter {
            URL(fileURLWithPath: $0.workspacePath).standardizedFileURL == workspace.path.standardizedFileURL
        }

        for activeSession in activeSessions {
            if let index = result.firstIndex(where: { $0.id == activeSession.id }) {
                result[index].activeSession = activeSession
            } else {
                result.append(
                    WorkspaceSession(activeSession: activeSession, workspaceID: workspace.id)
                )
            }
        }
        return result
    }

    private init(
        workspaces: [WorkspaceItem],
        devices: [PairedDevice],
        sessions: [ActiveSession],
        workspaceSessions: [WorkspaceSession],
        pairingRequests: [PairingRequest]
    ) {
        self.workspaces = workspaces
        self.devices = devices
        self.sessions = sessions
        self.workspaceSessions = workspaceSessions
        self.pairingRequests = pairingRequests
    }
}
