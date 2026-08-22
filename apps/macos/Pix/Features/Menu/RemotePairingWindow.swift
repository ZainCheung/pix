import CoreImage.CIFilterBuiltins
import SwiftUI

/// A focused, first-run friendly pairing guide.
///
/// Local pairing stays the default because it needs no relay configuration.
/// When a relay is active, the user can switch to a short-lived QR offer. The
/// same window remains the approval surface for requests arriving from either
/// transport.
struct AddDeviceWindow: View {
    @Environment(HostModel.self) private var model
    @State private var path: PairingPath = .local
    @State private var relayDraft = ""
    @State private var showRelayEditor = false
    @State private var now = Date()

    private let clock = Timer.publish(every: 1, on: .main, in: .common).autoconnect()

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                header

                if !model.pairingRequests.isEmpty {
                    approvalSection
                }

                if model.hasActiveRelay {
                    connectionPicker
                }

                if path == .remote, model.hasActiveRelay {
                    remoteConnection
                } else {
                    localConnection
                }

                relaySection
            }
            .padding(24)
            .frame(width: 460)
        }
        .onAppear {
            relayDraft = model.relayURL ?? ""
            showRelayEditor = !model.relayConfiguration.isConfigured
        }
        .onChange(of: model.relayConfiguration) { _, configuration in
            relayDraft = configuration.url ?? ""
            if configuration.isActive {
                showRelayEditor = false
            } else {
                path = .local
                showRelayEditor = true
            }
        }
        .onChange(of: path) { _, newPath in
            if newPath == .local {
                model.dismissRemotePairing()
            }
        }
        .onReceive(clock) {
            now = $0
            model.pruneExpiredPairingRequests(at: $0)
        }
        .onDisappear {
            model.dismissRemotePairing()
        }
        .task {
            model.start()
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: "iphone.badge.plus")
                .font(.system(size: 30, weight: .semibold))
                .foregroundStyle(.tint)
                .frame(width: 38, height: 38)

            VStack(alignment: .leading, spacing: 5) {
                Text(String(localized: "Add a device"))
                    .font(.title2.weight(.semibold))
                Text(String(localized: "Connect your iPhone to this Mac. Pairing stays protected by an approval step."))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var connectionPicker: some View {
        Picker(String(localized: "Connection"), selection: $path) {
            Text(String(localized: "Local network"))
                .tag(PairingPath.local)
            Text(String(localized: "Remote QR"))
                .tag(PairingPath.remote)
        }
        .pickerStyle(.segmented)
        .accessibilityLabel(String(localized: "Pairing connection"))
    }

    private var localConnection: some View {
        pairingPanel {
            sectionHeader(
                String(localized: "Connect on the same network"),
                String(localized: "No relay is needed. Keep both devices on the same Wi-Fi network."),
                systemImage: "wifi"
            )

            VStack(alignment: .leading, spacing: 14) {
                PairingStep(
                    number: 1,
                    title: String(localized: "Open Pix on your iPhone"),
                    detail: String(localized: "Choose Add device and allow Pix to look for nearby hosts.")
                )
                PairingStep(
                    number: 2,
                    title: String(localized: "Choose this Mac"),
                    detail: String(localized: "The Mac appears automatically while the local network is available.")
                )
                PairingStep(
                    number: 3,
                    title: String(localized: "Approve the request here"),
                    detail: String(localized: "Check the six-digit code before you approve the iPhone.")
                )
            }

            HStack(spacing: 8) {
                Circle()
                    .fill(model.status.tint.color)
                    .frame(width: 8, height: 8)
                Text(
                    model.pairingRequests.isEmpty
                        ? String(localized: "Waiting for your iPhone to request pairing")
                        : String(localized: "A pairing request is ready for your review")
                )
                .font(.callout)
                .foregroundStyle(.secondary)
            }
        }
    }

    private var remoteConnection: some View {
        pairingPanel {
            sectionHeader(
                String(localized: "Connect remotely"),
                String(localized: "Scan this one-time QR code from Pix on your iPhone."),
                systemImage: "qrcode"
            )

            if let offer = model.remotePairing, offer.expiresAt > now {
                VStack(spacing: 12) {
                    if let image = Self.qrImage(for: offer.qrPayload) {
                        Image(decorative: image, scale: 1)
                            .interpolation(.none)
                            .resizable()
                            .frame(width: 190, height: 190)
                            .accessibilityLabel(String(localized: "Remote pairing QR code"))
                    }

                    if !offer.joinCode.isEmpty {
                        Text(offer.joinCode)
                            .font(.system(size: 26, weight: .semibold, design: .monospaced))
                            .tracking(3)
                            .textSelection(.enabled)
                            .accessibilityLabel(String(localized: "Remote pairing code \(offer.joinCode)"))
                    }

                    Text(String(localized: "After the iPhone joins, compare the confirmation code and approve the request below."))
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .fixedSize(horizontal: false, vertical: true)

                    Text(String(localized: "Expires in \(remaining(until: offer.expiresAt))"))
                        .font(.system(.body, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .accessibilityLabel(String(localized: "Code expires in \(remaining(until: offer.expiresAt))"))

                    Button(String(localized: "Generate new code")) {
                        model.startRemotePairing()
                    }
                }
                .frame(maxWidth: .infinity)
            } else if let error = model.remotePairingError {
                Image(systemName: "wifi.exclamationmark")
                    .font(.system(size: 30))
                    .foregroundStyle(.orange)
                    .frame(maxWidth: .infinity)
                Text(error)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
                Button(String(localized: "Try again")) {
                    model.startRemotePairing()
                }
                .frame(maxWidth: .infinity)
            } else if model.remotePairing == nil {
                ProgressView()
                    .frame(maxWidth: .infinity)
                Text(String(localized: "Creating a secure, single-use pairing channel…"))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity)
                    .multilineTextAlignment(.center)
            } else {
                Image(systemName: "clock.badge.exclamationmark")
                    .font(.system(size: 30))
                    .foregroundStyle(.orange)
                    .frame(maxWidth: .infinity)
                Text(String(localized: "This code expired. Generate a new one to continue."))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity)
                    .multilineTextAlignment(.center)
                Button(String(localized: "Generate new code")) {
                    model.startRemotePairing()
                }
                .frame(maxWidth: .infinity)
            }
        }
        .task(id: path) {
            guard path == .remote, model.hasActiveRelay, model.remotePairing == nil else { return }
            model.startRemotePairing()
        }
    }

    private var approvalSection: some View {
        pairingPanel {
            sectionHeader(
                String(localized: "Approve a pairing request"),
                String(localized: "Verify the code shown on the iPhone before you trust this device."),
                systemImage: "person.crop.circle.badge.questionmark"
            )

            ForEach(model.pairingRequests) { request in
                approvalRow(request)
            }
        }
        .accessibilityIdentifier("pairing-approval-section")
    }

    private func approvalRow(_ request: PairingRequest) -> some View {
        let expired = request.expiresAt <= now

        return VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                Text(request.deviceName)
                    .font(.headline)
                Spacer()
                Text(expired ? String(localized: "Expired") : String(localized: "Pending"))
                    .font(.caption.weight(.medium))
                    .foregroundStyle(expired ? .red : .secondary)
            }

            Text(request.confirmationCode)
                .font(.system(size: 30, weight: .semibold, design: .monospaced))
                .tracking(5)
                .textSelection(.enabled)
                .accessibilityLabel(String(localized: "Confirmation code \(request.confirmationCode)"))

            Text(
                expired
                    ? String(localized: "Ask the iPhone to start pairing again.")
                    : String(localized: "Make sure this code matches the iPhone, then choose approve.")
            )
            .font(.callout)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            HStack {
                Button(String(localized: "Reject"), role: .destructive) {
                    model.reject(request)
                }
                .disabled(expired)

                Spacer()

                Button(String(localized: "Approve")) {
                    model.approve(request)
                }
                .buttonStyle(.borderedProminent)
                .disabled(expired)
            }
        }
        .padding(.top, 4)
    }

    private var relaySection: some View {
        VStack(alignment: .leading, spacing: 11) {
            Divider()

            sectionHeader(
                String(localized: "Remote connection"),
                String(localized: "Use a relay when the iPhone and Mac are not on the same network."),
                systemImage: "network"
            )

            if model.hasActiveRelay {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                    Text(String(localized: "Relay ready"))
                        .font(.callout.weight(.medium))
                    Spacer()
                    Button(String(localized: "Configure")) {
                        showRelayEditor = true
                    }
                    .buttonStyle(.link)
                }
                Text(model.relayURL ?? "")
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)

                if showRelayEditor {
                    relayEditor
                } else {
                    HStack {
                        Button(String(localized: "Use remote pairing")) {
                            path = .remote
                        }
                        Button(String(localized: "Disable")) {
                            model.disableRelay()
                            path = .local
                            showRelayEditor = true
                        }
                        .buttonStyle(.link)
                    }
                }
            } else if model.relayConfiguration.isConfigured {
                Text(String(localized: "A relay is saved but disabled. Enable it to show the remote QR option."))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                HStack {
                    Button(String(localized: "Enable relay")) {
                        model.enableRelay()
                    }
                    .buttonStyle(.bordered)
                    Button(String(localized: "Change")) {
                        showRelayEditor = true
                    }
                    .buttonStyle(.link)
                }
                if showRelayEditor {
                    relayEditor
                }
            } else {
                Text(String(localized: "Configure a relay below if you also need to pair while away from home."))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                relayEditor
            }
        }
    }

    private var relayEditor: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField(String(localized: "wss://relay.example.com"), text: $relayDraft)
                .textFieldStyle(.roundedBorder)
                .onSubmit {
                    model.configureRelay(relayDraft)
                }

            Text(String(localized: "Saving changes restarts the managed Host service to apply the relay."))
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            HStack {
                Button(model.isUpdatingRelay ? String(localized: "Saving…") : String(localized: "Save relay")) {
                    model.configureRelay(relayDraft)
                }
                .buttonStyle(.borderedProminent)
                .disabled(model.isUpdatingRelay || relayDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)

                if model.relayConfiguration.isConfigured {
                    Button(String(localized: "Remove"), role: .destructive) {
                        model.clearRelay()
                        path = .local
                    }
                    .buttonStyle(.link)
                    .disabled(model.isUpdatingRelay)
                }
            }

            if let error = model.relayError {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func pairingPanel<Content: View>(@ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 14, content: content)
            .padding(15)
            .background(.quaternary, in: RoundedRectangle(cornerRadius: 12))
    }

    private func sectionHeader(_ title: String, _ detail: String, systemImage: String) -> some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: systemImage)
                .foregroundStyle(.tint)
                .frame(width: 20)
            VStack(alignment: .leading, spacing: 3) {
                Text(title)
                    .font(.headline)
                Text(detail)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func remaining(until date: Date) -> String {
        let seconds = max(0, Int(date.timeIntervalSince(now)))
        return String(format: "%d:%02d", seconds / 60, seconds % 60)
    }

    private static func qrImage(for payload: String) -> CGImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(payload.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage else { return nil }
        let scaled = output.transformed(by: CGAffineTransform(scaleX: 10, y: 10))
        return CIContext().createCGImage(scaled, from: scaled.extent)
    }

    private enum PairingPath: Hashable {
        case local
        case remote
    }
}

private struct PairingStep: View {
    let number: Int
    let title: String
    let detail: String

    var body: some View {
        HStack(alignment: .top, spacing: 11) {
            Text(String(number))
                .font(.system(.caption, design: .monospaced).weight(.semibold))
                .foregroundStyle(.tint)
                .frame(width: 22, height: 22)
                .background(.tint.opacity(0.12), in: Circle())

            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.callout.weight(.medium))
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

/// Kept as a source-compatible name for callers from the original menu
/// implementation. The window is now the complete local and remote guide.
typealias RemotePairingWindow = AddDeviceWindow
