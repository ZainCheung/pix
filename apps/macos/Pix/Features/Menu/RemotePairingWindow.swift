import CoreImage.CIFilterBuiltins
import SwiftUI

/// Presents the two-minute remote pairing QR code.
///
/// The code carries rendezvous material only (relay endpoint, single-use
/// channel secret, host fingerprint). Approval still happens in this menu
/// bar app with the same six-digit confirmation as local pairing.
struct RemotePairingWindow: View {
    @Environment(HostModel.self) private var model
    @State private var now = Date()

    private let clock = Timer.publish(every: 1, on: .main, in: .common).autoconnect()

    var body: some View {
        VStack(spacing: 16) {
            if model.relayURL == nil {
                relayMissing
            } else if !model.pairingRequests.isEmpty {
                pendingApprovals
            } else if let offer = model.remotePairing, offer.expiresAt > now {
                activeOffer(offer)
            } else {
                expiredOrLoading
            }
        }
        .padding(24)
        .frame(width: 360)
        .onReceive(clock) { now = $0 }
        .onDisappear { model.dismissRemotePairing() }
    }

    /// The QR window is what the user is looking at while the phone joins.
    /// Pairing requests must appear here — the menu-bar extra is a `.menu`
    /// and is not on screen while this window is open.
    private var pendingApprovals: some View {
        VStack(spacing: 16) {
            Text(String(localized: "Approve this iPhone"))
                .font(.headline)
            ForEach(model.pairingRequests) { request in
                VStack(spacing: 10) {
                    Text(request.deviceName)
                        .font(.title3)
                    Text(request.confirmationCode)
                        .font(.system(size: 36, weight: .semibold, design: .monospaced))
                        .tracking(6)
                        .textSelection(.enabled)
                        .accessibilityLabel(String(localized: "Confirmation code \(request.confirmationCode)"))
                    Text(String(localized: "Confirm this code matches the iPhone, then approve."))
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                    HStack(spacing: 12) {
                        Button(String(localized: "Reject"), role: .destructive) {
                            model.reject(request)
                        }
                        Button(String(localized: "Approve")) {
                            model.approve(request)
                        }
                        .keyboardShortcut(.defaultAction)
                    }
                }
            }
        }
    }

    private func activeOffer(_ offer: RemotePairingOffer) -> some View {
        VStack(spacing: 16) {
            Text(String(localized: "Pair iPhone Remotely"))
                .font(.headline)
            Text(offer.joinCode)
                .font(.system(size: 36, weight: .semibold, design: .monospaced))
                .tracking(2)
                .textSelection(.enabled)
                .accessibilityLabel(String(localized: "Matching code \(offer.joinCode)"))
                .accessibilityIdentifier("remote-pairing-join-code")
            Text(String(localized: "In Pix on the iPhone, enter this matching code. You can scan the QR code instead if you prefer."))
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            if let image = Self.qrImage(for: offer.qrPayload) {
                Image(decorative: image, scale: 1)
                    .interpolation(.none)
                    .resizable()
                    .frame(width: 160, height: 160)
                    .accessibilityLabel(String(localized: "Remote pairing QR code"))
            }
            Text(remaining(until: offer.expiresAt))
                .font(.system(.body, design: .monospaced))
                .foregroundStyle(.secondary)
                .accessibilityLabel(String(localized: "Code expires in \(remaining(until: offer.expiresAt))"))
            Button(String(localized: "Generate New Code")) {
                model.startRemotePairing()
            }
        }
    }

    private var expiredOrLoading: some View {
        VStack(spacing: 16) {
            Text(String(localized: "Pair iPhone Remotely"))
                .font(.headline)
            if model.remotePairing == nil {
                ProgressView()
                Text(String(localized: "Creating a single-use pairing channel…"))
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                Image(systemName: "clock.badge.exclamationmark")
                    .font(.system(size: 36))
                    .foregroundStyle(.orange)
                Text(String(localized: "This code expired. Codes are single-use and last two minutes."))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                Button(String(localized: "Generate New Code")) {
                    model.startRemotePairing()
                }
            }
        }
        .task {
            if model.remotePairing == nil {
                model.startRemotePairing()
            }
        }
    }

    private var relayMissing: some View {
        VStack(spacing: 12) {
            Image(systemName: "network.slash")
                .font(.system(size: 36))
                .foregroundStyle(.secondary)
            Text(String(localized: "Remote pairing needs a relay"))
                .font(.headline)
            Text(String(localized: "Configure the relay endpoint first, for example: pix relay set wss://relay.example.com — then reopen this window."))
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .textSelection(.enabled)
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
}
