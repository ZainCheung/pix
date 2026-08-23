import AppKit
import SwiftUI

/// The first-run setup guide, mirroring the CLI wizard's recommended path:
/// verify Pi, choose remote access, authorize one workspace, and start the
/// background service. It commits through the same headless CLI commands the
/// terminal wizard uses, so both surfaces stay behaviorally identical.
struct SetupWindow: View {
    @Environment(HostModel.self) private var model
    @Environment(\.dismissWindow) private var dismissWindow
    @Environment(\.openWindow) private var openWindow

    private enum RelayMode: Hashable {
        case recommended
        case localOnly
        case custom
    }

    @State private var relayMode: RelayMode = .recommended
    @State private var customRelay = ""
    @State private var workspaceURL: URL?
    @State private var installService = true
    @State private var isRunning = false
    @State private var setupError: String?
    @State private var didComplete = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                header

                if didComplete {
                    completionSection
                } else {
                    piSection
                    relaySection
                    workspaceSection
                    serviceSection
                    if let setupError {
                        Text(setupError)
                            .foregroundStyle(.red)
                            .font(.callout)
                    }
                    runButton
                }
            }
            .padding(24)
            .frame(width: 460)
        }
        .onAppear {
            if model.piVersion == nil {
                refreshPi()
            }
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Set up Pix")
                .font(.title2.bold())
            Text("Prepare this Mac so you can use Pi from your phone.")
                .foregroundStyle(.secondary)
        }
    }

    // MARK: Pi

    private var piSection: some View {
        section("Pi", symbol: "terminal") {
            HStack {
                if let version = model.piVersion {
                    Label("Pi \(version)", systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                } else {
                    VStack(alignment: .leading, spacing: 4) {
                        Label("Pi was not found", systemImage: "exclamationmark.triangle.fill")
                            .foregroundStyle(.orange)
                        Text("Install Pi, or choose its executable to continue.")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer()
                if model.piVersion == nil {
                    Button("Choose…") { choosePi() }
                } else {
                    Button("Change…") { choosePi() }
                }
            }
        }
    }

    private func choosePi() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.prompt = String(localized: "Select")
        panel.message = String(localized: "Choose the Pi executable.")
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task { @MainActor in
            do {
                try await model.setPiExecutable(url)
            } catch {
                setupError = error.localizedDescription
            }
        }
    }

    private func refreshPi() {
        Task { @MainActor in
            _ = try? await model.refreshStatusOnly()
        }
    }

    // MARK: Relay

    private var relaySection: some View {
        section("Remote access", symbol: "antenna.radiowaves.left.and.right") {
            Picker("How should Pix reach this Mac when you're away?", selection: $relayMode) {
                Text("Pix Relay (recommended)").tag(RelayMode.recommended)
                Text("Local network only").tag(RelayMode.localOnly)
                Text("Custom relay…").tag(RelayMode.custom)
            }
            .pickerStyle(.radioGroup)
            .labelsHidden()

            Text("The relay carries end-to-end encrypted traffic; use it when your phone is off this network.")
                .font(.callout)
                .foregroundStyle(.secondary)

            if relayMode == .custom {
                TextField("wss://relay.example.com", text: $customRelay)
                    .textFieldStyle(.roundedBorder)
            }
        }
    }

    // MARK: Workspace

    private var workspaceSection: some View {
        section("Workspace", symbol: "folder") {
            HStack {
                if let workspaceURL {
                    Label(
                        workspaceURL.lastPathComponent.isEmpty
                            ? workspaceURL.path
                            : workspaceURL.lastPathComponent,
                        systemImage: "checkmark.circle.fill"
                    )
                    .foregroundStyle(.green)
                    .lineLimit(1)
                    .truncationMode(.middle)
                } else {
                    Text("Choose a folder Pi may use.")
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button(workspaceURL == nil ? "Choose…" : "Change…") { chooseWorkspace() }
            }
        }
    }

    private func chooseWorkspace() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = String(localized: "Authorize")
        panel.message = String(localized: "Choose a workspace Pi may use.")
        guard panel.runModal() == .OK, let url = panel.url else { return }
        workspaceURL = url.standardizedFileURL
    }

    // MARK: Service

    private var serviceSection: some View {
        section("Background service", symbol: "gearshape") {
            Toggle("Start Pix automatically when this Mac starts", isOn: $installService)
        }
    }

    // MARK: Run

    private var runButton: some View {
        Button {
            runSetup()
        } label: {
            HStack {
                if isRunning {
                    ProgressView()
                        .controlSize(.small)
                }
                Text("Set Up Pix")
                    .frame(maxWidth: .infinity)
            }
        }
        .controlSize(.large)
        .buttonStyle(.borderedProminent)
        .disabled(!canRun || isRunning)
    }

    private var canRun: Bool {
        guard model.piVersion != nil, let workspaceURL else { return false }
        if relayMode == .custom {
            return Self.isValidRelayURL(customRelay)
        }
        return true
    }

    static func isValidRelayURL(_ value: String) -> Bool {
        HostModel.normalizedRelayURL(value) != nil
    }

    private func runSetup() {
        setupError = nil
        isRunning = true
        let choice: HostModel.SetupRelayChoice = switch relayMode {
        case .recommended: .recommended
        case .localOnly: .localOnly
        case .custom: .custom(customRelay)
        }
        Task { @MainActor in
            defer { isRunning = false }
            do {
                try await model.applySetup(
                    relay: choice,
                    workspaceURL: workspaceURL!,
                    installService: installService
                )
                didComplete = true
            } catch {
                setupError = error.localizedDescription
            }
        }
    }

    // MARK: Completion

    private var completionSection: some View {
        VStack(alignment: .leading, spacing: 16) {
            Label("Pix is ready", systemImage: "checkmark.circle.fill")
                .font(.title3.bold())
                .foregroundStyle(.green)
            Text("This Mac is prepared for remote Pi access. Pair your phone to start.")
                .foregroundStyle(.secondary)
            HStack {
                Button {
                    dismissWindow()
                    openWindow(id: "add-device")
                    NSApp.activate(ignoringOtherApps: true)
                } label: {
                    Label("Pair a Device…", systemImage: "iphone.gen3")
                }
                .buttonStyle(.borderedProminent)
                Spacer()
                Button("Done") { dismissWindow() }
            }
        }
        .padding(.top, 8)
    }

    // MARK: Section helper

    private func section<Content: View>(
        _ title: LocalizedStringKey,
        symbol: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(title, systemImage: symbol)
                .font(.headline)
            content()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
