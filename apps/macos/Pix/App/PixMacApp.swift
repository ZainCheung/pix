import SwiftUI

@main
struct PixMacApp: App {
    @State private var model: HostModel
    @State private var updateController: UpdateController

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

        let updateController = MainActor.assumeIsolated {
            UpdateController()
        }
        _updateController = State(initialValue: updateController)
    }

    var body: some Scene {
        MenuBarExtra {
            HostMenuView()
                .environment(model)
                .environment(updateController)
        } label: {
            StatusItemLabel()
                .environment(model)
        }
        .menuBarExtraStyle(.menu)

        Window(String(localized: "Set Up Pix"), id: "setup") {
            SetupWindow()
                .environment(model)
                .environment(updateController)
        }
        .windowResizability(.contentSize)
        .defaultSize(width: 480, height: 480)
        .defaultPosition(.center)

        Window(String(localized: "Add Device"), id: "add-device") {
            AddDeviceWindow()
                .environment(model)
                .environment(updateController)
        }
        .windowResizability(.contentSize)
        .defaultSize(width: 460, height: 700)
        .defaultPosition(.center)

        Settings {
            SettingsView()
                .environment(model)
                .environment(updateController)
        }
    }
}
