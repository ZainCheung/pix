import Foundation
import Testing
@testable import Pix

@Test("host status exposes actionable setup state")
@MainActor
func hostModelStartsInSetupState() {
    let model = HostModel(configPath: URL(fileURLWithPath: "/tmp/pix-test-config.json"))
    #expect(model.status == .starting)
    #expect(model.workspaces.isEmpty)
    #expect(model.devices.isEmpty)
    #expect(model.inventoryRevision == 0)
}

@Test("menu lifecycle status can advance independently of its inventory snapshot")
func menuStatusPresentationReflectsReadyState() {
    let starting = HostMenuStatusPresentation(status: .starting, detail: nil)
    let ready = HostMenuStatusPresentation(status: .ready, detail: nil)

    #expect(starting.status == .starting)
    #expect(ready.status == .ready)
    #expect(ready.status.title == "Ready")
    #expect(starting.status.menuSymbolName == "clock")
    #expect(ready.status.menuSymbolName == "checkmark.circle.fill")
    #expect(HostStatus.failed("boom").tint == .danger)
    #expect(HostStatus.failed("boom").title == "Error")
    #expect(HostStatus.failed("boom").menuSymbolName == "xmark.circle.fill")
}

@Test("menu inventory hydrates only after reconciliation advances its revision")
func menuInventoryHydratesAfterInventoryRevision() {
    #expect(!HostMenuSnapshot.shouldHydrate(forInventoryRevision: 0))
    #expect(HostMenuSnapshot.shouldHydrate(forInventoryRevision: 1))
}

@Test("pairing requests preserve the six digit confirmation code")
@MainActor
func pairingRequestIsValueStable() {
    let request = PairingRequest(
        id: UUID(),
        deviceName: "Test iPhone",
        confirmationCode: "012345",
        expiresAt: Date(timeIntervalSince1970: 100)
    )
    #expect(request.confirmationCode == "012345")
}

@Test("expired pairing requests are removed from the approval surface")
@MainActor
func expiredPairingRequestsArePruned() {
    let model = HostModel(configPath: URL(fileURLWithPath: "/tmp/pix-test-config.json"))
    model.updatePairingRequests([
        PairingRequest(
            id: UUID(),
            deviceName: "Expired iPhone",
            confirmationCode: "012345",
            expiresAt: Date(timeIntervalSince1970: 100)
        ),
        PairingRequest(
            id: UUID(),
            deviceName: "Active iPhone",
            confirmationCode: "678901",
            expiresAt: Date(timeIntervalSince1970: 200)
        ),
    ])
    model.pruneExpiredPairingRequests(at: Date(timeIntervalSince1970: 150))
    #expect(model.pairingRequests.count == 1)
    #expect(model.pairingRequests.first?.deviceName == "Active iPhone")
}

@Test("device inventory parser keeps names and hides empty lists")
func parsesPairedDeviceTextInventory() {
    let devices = HostTextInventory.devices(
        from: """
        No paired devices.
        """
    )
    #expect(devices.isEmpty)

    let parsed = HostTextInventory.devices(
        from: """
        abcdef  Test iPhone
          paired 2026-08-13T00:00:00Z
        """
    )
    #expect(parsed.count == 1)
    #expect(parsed.first?.name == "Test iPhone")
    #expect(parsed.first?.id == "abcdef")
}

@Test("active sessions expose workspace names without host chat UI")
func activeSessionUsesWorkspaceName() {
    let session = ActiveSession(
        id: "session-1",
        workspacePath: "/tmp/pix",
        clients: 1,
        state: "running"
    )
    #expect(session.workspaceName == "pix")
    #expect(session.isRunning)
}

@Test("workspace session inventory decodes menu-safe summaries")
func parsesWorkspaceSessionInventory() {
    let workspaceID = UUID()
    let sessions = HostModel.parseWorkspaceSessions(
        from: """
        {"id":"session-1","title":"Fix menu hover","modified_at":"2026-08-23T10:00:00.123Z","message_count":4}
        {"id":"session-2","title":null,"modified_at":"2026-08-22T10:00:00Z","message_count":0}
        """,
        workspaceID: workspaceID
    )

    #expect(sessions.count == 2)
    #expect(sessions.first?.workspaceID == workspaceID)
    #expect(sessions.first?.displayTitle == "Fix menu hover")
    #expect(sessions.last?.displayTitle == "Untitled Session")
    #expect(sessions.first?.messageCount == 4)
}

@Test("CLI resolver finds a Pix binary in the GUI-visible PATH")
func resolvesPixFromPathWithoutShellEnvironment() throws {
    let root = FileManager.default.temporaryDirectory
        .appendingPathComponent("pix-host-model-\(UUID().uuidString)")
    let bin = root.appendingPathComponent("bin")
    try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: root) }

    let executable = bin.appendingPathComponent("pix")
    try Data("#!/bin/sh\n".utf8).write(to: executable)
    try FileManager.default.setAttributes(
        [.posixPermissions: NSNumber(value: Int16(0o755))],
        ofItemAtPath: executable.path
    )

    let resolved = try HostModel.resolvePixExecutable(
        environment: ["PATH": bin.path],
        homeDirectory: root.appendingPathComponent("home"),
        bundle: nil,
        searchLoginShell: false
    )
    #expect(resolved.standardizedFileURL == executable.standardizedFileURL)
}

@Test("CLI resolver prefers the embedded binary over a stale PATH install")
func resolvesEmbeddedPixBeforePath() throws {
    let root = FileManager.default.temporaryDirectory
        .appendingPathComponent("pix-host-model-\(UUID().uuidString)")
    let bundleURL = root.appendingPathComponent("Pix.bundle")
    let resources = bundleURL.appendingPathComponent("Contents/Resources")
    let staleBin = root.appendingPathComponent("stale-bin")
    try FileManager.default.createDirectory(at: resources, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: staleBin, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: root) }

    try Data(
        """
        <?xml version="1.0" encoding="UTF-8"?>
        <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
        <plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.pix.tests</string></dict></plist>
        """.utf8
    ).write(to: bundleURL.appendingPathComponent("Contents/Info.plist"))

    let embedded = resources.appendingPathComponent("pix")
    try Data("#!/bin/sh\n".utf8).write(to: embedded)
    try FileManager.default.setAttributes(
        [.posixPermissions: NSNumber(value: Int16(0o755))],
        ofItemAtPath: embedded.path
    )

    let stale = staleBin.appendingPathComponent("pix")
    try Data("#!/bin/sh\n".utf8).write(to: stale)
    try FileManager.default.setAttributes(
        [.posixPermissions: NSNumber(value: Int16(0o755))],
        ofItemAtPath: stale.path
    )

    guard let bundle = Bundle(path: bundleURL.path) else {
        Issue.record("Could not load the temporary test bundle")
        return
    }
    let resolved = try HostModel.resolvePixExecutable(
        environment: ["PATH": staleBin.path],
        homeDirectory: root.appendingPathComponent("home"),
        bundle: bundle,
        searchLoginShell: false
    )
    #expect(resolved.standardizedFileURL == embedded.standardizedFileURL)
}

@Test("CLI resolver honors an explicit development override before the bundle")
func resolvesExplicitPixOverrideBeforeBundle() throws {
    let root = FileManager.default.temporaryDirectory
        .appendingPathComponent("pix-host-model-\(UUID().uuidString)")
    let bundleURL = root.appendingPathComponent("Pix.bundle")
    let resources = bundleURL.appendingPathComponent("Contents/Resources")
    let overrideBin = root.appendingPathComponent("override-bin")
    try FileManager.default.createDirectory(at: resources, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: overrideBin, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: root) }

    try Data(
        """
        <?xml version="1.0" encoding="UTF-8"?>
        <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
        <plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.pix.tests</string></dict></plist>
        """.utf8
    ).write(to: bundleURL.appendingPathComponent("Contents/Info.plist"))

    let embedded = resources.appendingPathComponent("pix")
    try Data("#!/bin/sh\n".utf8).write(to: embedded)
    try FileManager.default.setAttributes(
        [.posixPermissions: NSNumber(value: Int16(0o755))],
        ofItemAtPath: embedded.path
    )

    let override = overrideBin.appendingPathComponent("pix")
    try Data("#!/bin/sh\n".utf8).write(to: override)
    try FileManager.default.setAttributes(
        [.posixPermissions: NSNumber(value: Int16(0o755))],
        ofItemAtPath: override.path
    )

    guard let bundle = Bundle(path: bundleURL.path) else {
        Issue.record("Could not load the temporary test bundle")
        return
    }
    let resolved = try HostModel.resolvePixExecutable(
        environment: ["PIX_CLI": override.path, "PATH": "/usr/bin"],
        homeDirectory: root.appendingPathComponent("home"),
        bundle: bundle,
        searchLoginShell: false
    )
    #expect(resolved.standardizedFileURL == override.standardizedFileURL)
}

@Test("CLI resolver includes the mise shim fallback")
func resolvesPixFromMiseShimFallback() throws {
    let root = FileManager.default.temporaryDirectory
        .appendingPathComponent("pix-host-model-\(UUID().uuidString)")
    let shimDirectory = root.appendingPathComponent(".local/share/mise/shims")
    try FileManager.default.createDirectory(at: shimDirectory, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: root) }

    let executable = shimDirectory.appendingPathComponent("pix")
    try Data("#!/bin/sh\n".utf8).write(to: executable)
    try FileManager.default.setAttributes(
        [.posixPermissions: NSNumber(value: Int16(0o755))],
        ofItemAtPath: executable.path
    )

    let resolved = try HostModel.resolvePixExecutable(
        environment: ["PATH": "/usr/bin"],
        homeDirectory: root,
        bundle: nil,
        searchLoginShell: false
    )
    #expect(resolved.standardizedFileURL == executable.standardizedFileURL)
}

@Test("default config path prefers the unified host config")
func prefersUnifiedConfigPath() throws {
    let root = FileManager.default.temporaryDirectory
        .appendingPathComponent("pix-host-model-\(UUID().uuidString)")
    let current = root.appendingPathComponent(".config/pix/config.json")
    let legacy = root.appendingPathComponent("Library/Application Support/Pix/config.json")
    defer { try? FileManager.default.removeItem(at: root) }

    #expect(HostModel.defaultConfigPath(homeDirectory: root) == current)

    try FileManager.default.createDirectory(
        at: legacy.deletingLastPathComponent(),
        withIntermediateDirectories: true
    )
    try Data("{}".utf8).write(to: legacy)
    #expect(HostModel.defaultConfigPath(homeDirectory: root) == legacy)

    try FileManager.default.createDirectory(
        at: current.deletingLastPathComponent(),
        withIntermediateDirectories: true
    )
    try Data("{}".utf8).write(to: current)
    #expect(HostModel.defaultConfigPath(homeDirectory: root) == current)
}

@Test("relay status parser distinguishes enabled and disabled endpoints")
func parsesRelayStatus() {
    let enabled = HostModel.parseRelayConfiguration(
        from: "relay: wss://relay.example.com (enabled)\n"
    )
    #expect(enabled.url == "wss://relay.example.com")
    #expect(enabled.isActive)

    let disabled = HostModel.parseRelayConfiguration(
        from: "relay: wss://relay.example.com (disabled)\n"
    )
    #expect(disabled.url == "wss://relay.example.com")
    #expect(disabled.isConfigured)
    #expect(!disabled.isActive)

    #expect(HostModel.parseRelayConfiguration(from: "relay: not configured\n") == .none)

    let json = HostModel.parseRelayConfiguration(
        from: """
        {"schema_version":1,"ok":true,"command":"relay.show","data":{"url":"wss://relay.example.com","enabled":true,"configured":true,"service_restart_required":false}}
        """
    )
    #expect(json.url == "wss://relay.example.com")
    #expect(json.isActive)
}

@Test("native app always invokes the machine-readable non-interactive CLI")
func buildsHeadlessCLIArguments() {
    #expect(
        HostModel.headlessArguments(["workspace", "list"])
            == ["--output", "json", "--no-input", "workspace", "list"]
    )
}

@Test("relay URL validation only accepts credential-free WebSocket endpoints")
func validatesRelayURL() {
    #expect(HostModel.normalizedRelayURL(" wss://relay.example.com ") == "wss://relay.example.com")
    #expect(HostModel.normalizedRelayURL("ws://127.0.0.1:8787") == "ws://127.0.0.1:8787")
    #expect(HostModel.normalizedRelayURL("wss://relay.example.com/edge?region=cn") == "wss://relay.example.com/edge?region=cn")
    #expect(HostModel.normalizedRelayURL("https://relay.example.com") == nil)
    #expect(HostModel.normalizedRelayURL("wss://user:secret@relay.example.com") == nil)
    #expect(HostModel.normalizedRelayURL("wss://relay.example.com/with space") == nil)
    #expect(HostModel.normalizedRelayURL("not a URL") == nil)
}

@Test("first-run detection follows the status envelope's config state")
func firstRunDetectionFollowsConfigState() {
    let missing = HostModel.isConfiguredStatus(
        from: """
        {"schema_version":1,"ok":true,"command":"status","data":{"config_state":"missing","pi":{"source":"path"},"devices":0,"workspaces":0}}
        """
    )
    #expect(missing == false)

    let ready = HostModel.isConfiguredStatus(
        from: """
        {"schema_version":1,"ok":true,"command":"status","data":{"config_state":"ready","pi":{"source":"path","version":"0.84.2"},"devices":1,"workspaces":2}}
        """
    )
    #expect(ready == true)

    #expect(HostModel.isConfiguredStatus(from: "Pix status\n  config: missing") == nil)
}

@Test("guided setup recommends the product relay")
func setupRecommendsProductRelay() {
    #expect(HostModel.defaultRelayURL == "wss://pix-relay.zaincheung-255.workers.dev")
    #expect(HostModel.normalizedRelayURL(HostModel.defaultRelayURL) != nil)
    #expect(HostModel.normalizedRelayURL("not a url") == nil)
}

@Test("socket device lists advance the menu inventory revision")
@MainActor
func socketDeviceListAdvancesMenuInventory() throws {
    let model = HostModel()
    let before = model.inventoryRevision
    let event = try JSONDecoder().decode(
        ServiceEvent.self,
        from: Data(
            """
            {"type":"device_list","devices":[{"id":"abc","name":"iPhone","paired_at":"2026-08-24T00:00:00Z"}]}
            """.utf8
        )
    )
    model.apply(event)
    #expect(model.devices.count == 1)
    #expect(model.devices.first?.name == "iPhone")
    #expect(model.inventoryRevision == before + 1)
}
