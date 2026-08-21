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
        workspacePath: "/Users/zain/code/pix",
        clients: 1,
        state: "running"
    )
    #expect(session.workspaceName == "pix")
    #expect(session.isRunning)
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
