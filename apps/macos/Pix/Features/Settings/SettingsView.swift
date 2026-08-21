import SwiftUI

struct SettingsView: View {
    @Environment(HostModel.self) private var model

    var body: some View {
        TabView {
            GeneralSettingsView()
                .environment(model)
                .tabItem { Label(String(localized: "General"), systemImage: "gearshape") }
            WorkspacesSettingsView()
                .environment(model)
                .tabItem { Label(String(localized: "Workspaces"), systemImage: "folder") }
            DevicesSettingsView()
                .environment(model)
                .tabItem { Label(String(localized: "Devices"), systemImage: "iphone.gen3") }
        }
        .frame(width: 560, height: 360)
    }
}

private struct GeneralSettingsView: View {
    @Environment(HostModel.self) private var model

    var body: some View {
        Form {
            Section(String(localized: "Host")) {
                LabeledContent(String(localized: "Status"), value: model.status.title)
                LabeledContent(String(localized: "Pi version"), value: model.piVersion ?? String(localized: "Not detected"))
                LabeledContent(String(localized: "Pi executable"), value: model.piExecutable ?? String(localized: "Not detected"))
                HStack {
                    Button(String(localized: "Choose Pi…")) { model.selectPiExecutable() }
                    Button(String(localized: "Use PATH")) { model.clearPiExecutable() }
                        .disabled(model.piExecutable == nil)
                }
            }
            Section(String(localized: "Remote access")) {
                LabeledContent(
                    String(localized: "Relay"),
                    value: model.relayURL ?? String(localized: "Not configured")
                )
                Text(String(localized: "The relay carries only end-to-end encrypted frames. Configure it with: pix relay set wss://your-relay — remote pairing and away-from-home access activate automatically."))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
            Section(String(localized: "Startup")) {
                Toggle(
                    String(localized: "Launch at login"),
                    isOn: Binding(
                        get: { model.launchAtLoginEnabled },
                        set: { model.setLaunchAtLogin($0) }
                    )
                )
            }
            Section(String(localized: "Privacy")) {
                Text(String(localized: "Pix keeps Pi sessions and conversation history on this Mac. The iPhone receives only authorized workspace and session data."))
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .padding()
    }
}

private struct WorkspacesSettingsView: View {
    @Environment(HostModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            List {
                ForEach(model.workspaces) { workspace in
                    HStack {
                        Image(systemName: workspace.isAvailable ? "folder.fill" : "folder.badge.questionmark")
                            .foregroundStyle(workspace.isAvailable ? .blue : .orange)
                        VStack(alignment: .leading) {
                            Text(workspace.name)
                            Text(workspace.path.path)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .textSelection(.enabled)
                        }
                        Spacer()
                        Button(String(localized: "Remove")) { model.removeWorkspace(workspace) }
                            .buttonStyle(.borderless)
                    }
                }
            }
            HStack {
                Spacer()
                Button(String(localized: "Authorize Workspace…")) { model.addWorkspace() }
                    .keyboardShortcut("a", modifiers: [.command])
            }
        }
        .padding()
    }
}

private struct DevicesSettingsView: View {
    @Environment(HostModel.self) private var model

    var body: some View {
        List {
            if model.devices.isEmpty {
                ContentUnavailableView(
                    String(localized: "No paired devices"),
                    systemImage: "iphone.gen3",
                    description: Text(String(localized: "Start pairing from the iPhone, then approve it here."))
                )
            } else {
                ForEach(model.devices) { device in
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(device.name)
                            Text(device.id)
                                .font(.system(.caption, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .textSelection(.enabled)
                        }
                        Spacer()
                        Button(String(localized: "Revoke"), role: .destructive) {
                            model.revoke(device)
                        }
                        .buttonStyle(.borderless)
                    }
                }
            }
        }
        .padding()
    }
}
