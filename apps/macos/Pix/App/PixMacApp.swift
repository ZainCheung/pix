import SwiftUI

@main
struct PixMacApp: App {
    @State private var model: HostModel

    init() {
        // Menu-style MenuBarExtra content only exists while the menu is
        // open, so the host service must start with the app itself or the
        // phone cannot connect until the user happens to click the icon.
        let model = MainActor.assumeIsolated {
            let host = HostModel()
            host.start()
            return host
        }
        _model = State(initialValue: model)
    }

    var body: some Scene {
        MenuBarExtra {
            HostMenuView()
                .environment(model)
        } label: {
            StatusLabel(status: model.status)
        }
        .menuBarExtraStyle(.menu)

        Window(String(localized: "Pair iPhone Remotely"), id: "remote-pairing") {
            RemotePairingWindow()
                .environment(model)
        }
        .windowResizability(.contentSize)
        .defaultPosition(.center)

        Settings {
            SettingsView()
                .environment(model)
        }
    }
}
