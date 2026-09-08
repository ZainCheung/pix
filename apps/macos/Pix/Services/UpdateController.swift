import Foundation
import Observation
import Sparkle

/// Owns the application's single Sparkle updater for the lifetime of Pix.
///
/// Menu-bar content is rebuilt every time it opens, so this object must live
/// at the App/Scene level instead of being created by a menu view. Sparkle's
/// standard controller owns the update window, download, signature
/// verification, install, and relaunch flow.
@Observable
@MainActor
final class UpdateController {
    @ObservationIgnored
    let updaterController: SPUStandardUpdaterController
    @ObservationIgnored
    private var canCheckForUpdatesObservation: NSKeyValueObservation?

    /// Stored so Swift Observation can invalidate views when Sparkle's KVO
    /// property changes. Reading the property through a computed accessor
    /// would not create an Observation dependency on the KVO publisher.
    private(set) var canCheckForUpdates: Bool

    init() {
        let controller = SPUStandardUpdaterController(
            startingUpdater: true,
            updaterDelegate: nil,
            userDriverDelegate: nil
        )
        updaterController = controller
        canCheckForUpdates = controller.updater.canCheckForUpdates
        canCheckForUpdatesObservation = nil
        canCheckForUpdatesObservation = controller.updater.observe(
            \.canCheckForUpdates,
            options: [.initial, .new]
        ) { [weak self] updater, _ in
            Task { @MainActor [weak self] in
                self?.canCheckForUpdates = updater.canCheckForUpdates
            }
        }
    }

    /// Mirrors Sparkle's persisted automatic-check preference. The initial
    /// default comes from `SUEnableAutomaticChecks` in Info.plist; subsequent
    /// changes are persisted by Sparkle itself.
    var automaticallyChecksForUpdates: Bool {
        get { updaterController.updater.automaticallyChecksForUpdates }
        set { updaterController.updater.automaticallyChecksForUpdates = newValue }
    }

    /// Exposed for the next update-settings phase, but intentionally not
    /// surfaced in the first release. `SUAllowsAutomaticUpdates` currently
    /// disables automatic installation in the standard Sparkle UI.
    var automaticallyDownloadsUpdates: Bool {
        get { updaterController.updater.automaticallyDownloadsUpdates }
        set { updaterController.updater.automaticallyDownloadsUpdates = newValue }
    }

    var currentVersion: String {
        value(for: "CFBundleShortVersionString") ?? String(localized: "Unknown")
    }

    var currentBuildVersion: String {
        value(for: "CFBundleVersion") ?? String(localized: "Unknown")
    }

    var currentVersionDisplay: String {
        "\(currentVersion) (\(currentBuildVersion))"
    }

    /// Opens Sparkle's standard update UI. Sparkle handles checking,
    /// downloading, EdDSA verification, installation, and relaunching the
    /// app in the original bundle location.
    func checkForUpdates() {
        updaterController.checkForUpdates(nil)
    }

    private func value(for key: String) -> String? {
        guard let value = Bundle.main.object(forInfoDictionaryKey: key) as? String,
              !value.isEmpty,
              !value.hasPrefix("$(")
        else {
            return nil
        }
        return value
    }
}
