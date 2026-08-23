import AppKit
import SwiftUI

/// The first-run setup guide as a stepped assistant. One decision per page,
/// a review step, and a completion page that hands off to pairing. It commits
/// through the same headless CLI commands the terminal wizard uses.
struct SetupWindow: View {
    @Environment(HostModel.self) private var model
    @Environment(\.dismissWindow) private var dismissWindow
    @Environment(\.openWindow) private var openWindow

    private enum Step: Int, CaseIterable {
        case pi
        case remoteAccess
        case workspace
        case review

        var title: LocalizedStringKey {
            switch self {
            case .pi: "Check Pi"
            case .remoteAccess: "Remote access"
            case .workspace: "Workspace"
            case .review: "Review"
            }
        }

        var headline: LocalizedStringKey {
            switch self {
            case .pi: "Pix runs on Pi"
            case .remoteAccess: "How should your phone reach this Mac?"
            case .workspace: "Choose a folder Pi may use"
            case .review: "Everything in place"
            }
        }

        var detail: LocalizedStringKey {
            switch self {
            case .pi: "Pix found the Pi CLI on this Mac and verified its version."
            case .remoteAccess: "The relay carries end-to-end encrypted traffic when your phone is away from this network."
            case .workspace: "Pix never browses your Mac; Pi only sees folders you authorize here."
            case .review: "You can change any of this later from the Pix menu."
            }
        }
    }

    private enum RelayMode: Hashable {
        case relay
        case localOnly
        case custom
    }

    @State private var step: Step = .pi
    @State private var relayMode: RelayMode = .relay
    @State private var customRelay = ""
    @State private var workspaceURL: URL?
    @State private var installService = true
    @State private var isRunning = false
    @State private var setupError: String?
    @State private var didComplete = false
    @State private var didChoosePi = false

    var body: some View {
        VStack(spacing: 0) {
            if didComplete {
                completionPage
            } else {
                wizard
            }
        }
        .frame(width: 480, height: 480)
        .onAppear {
            if model.piVersion == nil {
                Task { @MainActor in _ = try? await model.refreshStatusOnly() }
            }
        }
    }

    // MARK: Wizard chrome

    private var wizard: some View {
        VStack(spacing: 0) {
            header
            Divider()
            Group {
                switch step {
                case .pi: piPage
                case .remoteAccess: remoteAccessPage
                case .workspace: workspacePage
                case .review: reviewPage
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .animation(.easeInOut(duration: 0.18), value: step)
            Spacer(minLength: 0)
            if let setupError {
                Text(setupError)
                    .foregroundStyle(.red)
                    .font(.callout)
                    .padding(.horizontal, 24)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            footer
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                Text("Set up Pix")
                    .font(.title2.bold())
                Spacer()
                if !didComplete {
                    stepIndicator
                }
            }
            Text(step.headline)
                .font(.headline)
            Text(step.detail)
                .font(.callout)
                .foregroundStyle(.secondary)
        }
        .padding(24)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var stepIndicator: some View {
        HStack(spacing: 6) {
            ForEach(Step.allCases, id: \.rawValue) { candidate in
                Circle()
                    .fill(candidate.rawValue <= step.rawValue
                        ? Color.accentColor
                        : Color.secondary.opacity(0.3))
                    .frame(width: 7, height: 7)
            }
        }
        .accessibilityLabel("Step \(step.rawValue + 1) of \(Step.allCases.count)")
    }

    // MARK: Pages

    private var piPage: some View {
        page {
            if let version = model.piVersion {
                statusCard(
                    symbol: "checkmark.circle.fill",
                    tint: .green,
                    title: "Pi \(version)",
                    caption: model.piExecutablePath
                )
            } else if didChoosePi {
                statusCard(
                    symbol: "hourglass",
                    tint: .orange,
                    title: String(localized: "Still checking Pi…"),
                    caption: String(localized: "Pick the executable Pix should run.")
                )
            } else {
                VStack(spacing: 12) {
                    statusCard(
                        symbol: "exclamationmark.triangle.fill",
                        tint: .orange,
                        title: String(localized: "Pi was not found"),
                        caption: String(
                            localized: "Install Pi, or point Pix at the executable below."
                        )
                    )
                    Button {
                        choosePi()
                    } label: {
                        Label(String(localized: "Choose Pi executable…"), systemImage: "folder")
                    }
                }
            }
        }
    }

    private var remoteAccessPage: some View {
        page {
            VStack(spacing: 10) {
                choiceRow(
                    title: String(localized: "Pix Relay"),
                    caption: String(localized: "Recommended — works from any network"),
                    symbol: "antenna.radiowaves.left.and.right",
                    isSelected: relayMode == .relay
                ) { relayMode = .relay }

                choiceRow(
                    title: String(localized: "Local network only"),
                    caption: String(localized: "Your phone must share this Mac's network"),
                    symbol: "wifi",
                    isSelected: relayMode == .localOnly
                ) { relayMode = .localOnly }

                choiceRow(
                    title: String(localized: "Custom relay"),
                    caption: String(localized: "Point at a relay you operate"),
                    symbol: "server.rack",
                    isSelected: relayMode == .custom
                ) { relayMode = .custom }

                if relayMode == .custom {
                    TextField("wss://relay.example.com", text: $customRelay)
                        .textFieldStyle(.roundedBorder)
                }

                Text(
                    String(
                        localized: "With the relay on, pair your phone while it shares this Mac's network: it learns both routes and switches automatically."
                    )
                )
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(.top, 4)
            }
        }
    }

    private var workspacePage: some View {
        page {
            VStack(spacing: 14) {
                if let workspaceURL {
                    HStack(spacing: 12) {
                        Image(systemName: "checkmark.circle.fill")
                            .foregroundStyle(.green)
                            .font(.title3)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(
                                workspaceURL.lastPathComponent.isEmpty
                                    ? workspaceURL.path
                                    : workspaceURL.lastPathComponent
                            )
                            .font(.body.weight(.medium))
                            .lineLimit(1)
                            .truncationMode(.middle)
                            Text(workspaceURL.path)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                        Spacer()
                    }
                    .padding(14)
                    .background(.quaternary, in: RoundedRectangle(cornerRadius: 10))
                }
                Button {
                    chooseWorkspace()
                } label: {
                    Label(
                        workspaceURL == nil
                            ? String(localized: "Choose folder…")
                            : String(localized: "Choose a different folder…"),
                        systemImage: "folder.badge.plus"
                    )
                }
                .controlSize(.large)
            }
        }
    }

    private var reviewPage: some View {
        page(alignment: .leading) {
            VStack(spacing: 0) {
                reviewRow("Pi", value: model.piVersion.map { "Pi \($0)" } ?? "—")
                Divider()
                reviewRow("Remote access", value: relaySummary)
                Divider()
                reviewRow("Workspace", value: workspaceURL.map { $0.lastPathComponent } ?? "—")
                Divider()
                Toggle(isOn: $installService) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Start Pix automatically")
                        Text("Runs in the background so your phone can always connect.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                .padding(.vertical, 10)
            }
            .padding(.horizontal, 4)
        }
    }

    private var relaySummary: String {
        switch relayMode {
        case .relay: HostModel.defaultRelayURL
        case .localOnly: String(localized: "Local network only")
        case .custom: customRelay.isEmpty ? "—" : customRelay
        }
    }

    // MARK: Completion

    private var completionPage: some View {
        VStack(spacing: 20) {
            Spacer()
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 52))
                .foregroundStyle(.green)
            Text("Pix is ready")
                .font(.title2.bold())
            Text("This Mac is prepared for remote Pi access. Pair your phone to start.")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            tipCard
            Spacer()
            HStack {
                Button {
                    dismissWindow()
                    openWindow(id: "add-device")
                    NSApp.activate(ignoringOtherApps: true)
                } label: {
                    Label("Pair a Device…", systemImage: "iphone.gen3")
                        .frame(maxWidth: .infinity)
                }
                .controlSize(.large)
                .buttonStyle(.borderedProminent)

                Button("Done") { dismissWindow() }
                    .controlSize(.large)
            }
        }
        .padding(24)
    }

    private var tipCard: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "lightbulb")
                .foregroundStyle(.yellow)
            Text(
                String(
                    localized: "Pair while your phone shares this Mac's network. It then uses the fast local route here and the relay everywhere else."
                )
            )
            .font(.callout)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary, in: RoundedRectangle(cornerRadius: 10))
    }

    // MARK: Footer

    private var footer: some View {
        HStack {
            if step != .pi {
                Button("Back") {
                    withAnimation { step = Step(rawValue: step.rawValue - 1)! }
                }
            }
            Spacer()
            if step == .review {
                Button {
                    runSetup()
                } label: {
                    HStack {
                        if isRunning {
                            ProgressView().controlSize(.small)
                        }
                        Text("Set Up Pix")
                    }
                }
                .controlSize(.large)
                .buttonStyle(.borderedProminent)
                .disabled(!canFinish || isRunning)
            } else {
                Button("Continue") {
                    withAnimation { step = Step(rawValue: step.rawValue + 1)! }
                }
                .controlSize(.large)
                .buttonStyle(.borderedProminent)
                .disabled(!canContinue)
            }
        }
        .padding(24)
    }

    private var canContinue: Bool {
        switch step {
        case .pi: model.piVersion != nil
        case .remoteAccess: relayMode != .custom || Self.isValidRelayURL(customRelay)
        case .workspace, .review: workspaceURL != nil
        }
    }

    private var canFinish: Bool {
        canContinue
    }

    // MARK: Actions

    private func choosePi() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.prompt = String(localized: "Select")
        panel.message = String(localized: "Choose the Pi executable.")
        guard panel.runModal() == .OK, let url = panel.url else { return }
        didChoosePi = true
        setupError = nil
        Task { @MainActor in
            do {
                try await model.setPiExecutable(url)
            } catch {
                setupError = error.localizedDescription
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

    static func isValidRelayURL(_ value: String) -> Bool {
        HostModel.normalizedRelayURL(value) != nil
    }

    private func runSetup() {
        setupError = nil
        isRunning = true
        let choice: HostModel.SetupRelayChoice = switch relayMode {
        case .relay: .recommended
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
                withAnimation { didComplete = true }
            } catch {
                setupError = error.localizedDescription
            }
        }
    }

    // MARK: Pieces

    private func page<Content: View>(
        alignment: HorizontalAlignment = .center,
        @ViewBuilder content: () -> Content
    ) -> some View {
        ScrollView {
            VStack(alignment: alignment, spacing: 14) {
                content()
            }
            .padding(24)
            .frame(maxWidth: .infinity)
        }
    }

    private func statusCard(
        symbol: String,
        tint: Color,
        title: String,
        caption: String?
    ) -> some View {
        HStack(spacing: 12) {
            Image(systemName: symbol)
                .font(.title3)
                .foregroundStyle(tint)
            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(.body.weight(.medium))
                if let caption, !caption.isEmpty {
                    Text(caption)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .truncationMode(.middle)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary, in: RoundedRectangle(cornerRadius: 10))
    }

    private func choiceRow(
        title: String,
        caption: String,
        symbol: String,
        isSelected: Bool,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: symbol)
                    .font(.title3)
                    .foregroundStyle(isSelected ? Color.accentColor : .secondary)
                    .frame(width: 26)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.body.weight(.semibold))
                    Text(caption)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Image(systemName: isSelected ? "largecircle.fill.circle" : "circle")
                    .foregroundStyle(isSelected ? Color.accentColor : .secondary)
            }
            .padding(12)
            .contentShape(Rectangle())
            .background(
                isSelected
                    ? Color.accentColor.opacity(0.12)
                    : Color.clear,
                in: RoundedRectangle(cornerRadius: 10)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 10)
                    .strokeBorder(
                        isSelected ? Color.accentColor.opacity(0.5) : Color.secondary.opacity(0.25)
                    )
            )
        }
        .buttonStyle(.plain)
    }

    private func reviewRow(_ title: LocalizedStringKey, value: String) -> some View {
        HStack {
            Text(title).foregroundStyle(.secondary)
            Spacer()
            Text(value)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .padding(.vertical, 10)
    }
}
