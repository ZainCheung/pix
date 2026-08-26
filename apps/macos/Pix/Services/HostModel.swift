import AppKit
import Darwin
import Foundation
import Observation
import ServiceManagement

@Observable
@MainActor
final class HostModel {
    private(set) var status: HostStatus = .starting
    /// False until the CLI reports a host configuration file. Mirrors the
    /// CLI home screen's first-run state: setup guidance appears only then.
    private(set) var isConfigured = true
    private(set) var piVersion: String?
    private(set) var piExecutablePath: String?
    private(set) var piExecutable: String?
    private(set) var workspaces: [WorkspaceItem] = []
    private(set) var devices: [PairedDevice] = []
    private(set) var sessions: [ActiveSession] = []
    private(set) var workspaceSessions: [WorkspaceSession] = []
    /// Increments after the initial CLI-backed workspace/device reconciliation
    /// completes. Menu surfaces use this boundary instead of the host service's
    /// early `ready` event, which can arrive before the CLI inventory finishes.
    private(set) var inventoryRevision = 0
    private(set) var pairingRequests: [PairingRequest] = []
    private(set) var launchAtLoginEnabled = false
    private(set) var lastDiagnostic: String?
    /// Relay endpoint and enabled state reported by the host configuration.
    /// The channel secret is intentionally not represented in the macOS model.
    private(set) var relayConfiguration = RelayConfiguration.none
    private(set) var relayError: String?
    private(set) var isUpdatingRelay = false
    /// Active remote pairing offer awaiting QR presentation.
    private(set) var remotePairing: RemotePairingOffer?
    private(set) var remotePairingError: String?
    var relayURL: String? {
        relayConfiguration.url
    }

    var relayEnabled: Bool {
        relayConfiguration.isEnabled
    }

    var hasActiveRelay: Bool {
        relayConfiguration.isActive
    }

    private let configPath: URL
    private var didStart = false
    private var userStopped = false
    private var serviceEvents: UnixSocketConnection?
    private var serviceBuffer = Data()
    private var restartAttempts = 0
    private var isRequestingRemotePairing = false
    private var lifecycleTask: Task<Void, Never>?
    private var refreshQueued = false
    /// The CLI is resolved once for the lifetime of the model so the doctor
    /// check, inventory commands, and platform-managed Host service all use
    /// the same executable. An explicit refresh clears this cache.
    private var resolvedPixExecutable: URL?

    init(configPath: URL? = nil) {
        self.configPath = configPath
            ?? Self.defaultConfigPath()
    }

    func start() {
        guard !didStart else { return }
        didStart = true
        userStopped = false
        status = .starting

        lifecycleTask = Task { @MainActor [weak self] in
            guard let self else { return }
            await self.startLifecycle()
        }
    }

    private func startLifecycle() async {
        defer {
            lifecycleTask = nil
            let shouldRefresh = refreshQueued
            refreshQueued = false
            if !shouldRefresh, isUpdatingRelay, status == .ready {
                isUpdatingRelay = false
                relayError = nil
            }
            if shouldRefresh {
                refresh()
            }
        }
        do {
            let output = try await runPix(arguments: [
                "--config",
                configPath.path,
                "status",
            ])
            isConfigured = Self.isConfiguredStatus(from: output) ?? true
            piVersion = parseVersion(from: output)
            guard isConfigured else {
                // First run: hold in setup state without touching the
                // service or inventories; the setup guide drives the rest.
                status = .needsSetup(String(localized: "Pix is not set up on this computer."))
                return
            }
            guard piVersion != nil else {
                throw HostModelError.commandFailed(
                    "Pi was not found on this host; install Pi or set its path with `pix pi set`."
                )
            }
            launchAtLoginEnabled = SMAppService.mainApp.status == .enabled
            try await startHostService()
            await loadHostInventory()
            status = .ready
        } catch {
            status = .needsSetup(error.localizedDescription)
            if isUpdatingRelay, !refreshQueued {
                relayError = error.localizedDescription
                isUpdatingRelay = false
            }
        }
    }

    enum SetupRelayChoice {
        case recommended
        case localOnly
        case custom(String)
    }

    /// The product relay the CLI setup offers by default.
    static let defaultRelayURL = "wss://pix-relay.zaincheung-255.workers.dev"

    /// Applies the guided first-run choices through the same headless CLI
    /// commands the terminal wizard commits, then adopts the running host.
    func applySetup(
        relay: SetupRelayChoice,
        workspaceURL: URL,
        installService: Bool
    ) async throws {
        switch relay {
        case .recommended:
            _ = try await runPix(arguments: ["relay", "set", Self.defaultRelayURL])
        case .custom(let url):
            _ = try await runPix(arguments: ["relay", "set", url])
        case .localOnly:
            break
        }
        _ = try await runPix(arguments: ["workspace", "add", workspaceURL.path])
        isConfigured = true
        if installService {
            try await startHostService()
        }
        await loadHostInventory()
        status = .ready
    }

    /// Pins an explicit Pi executable (`pix pi set`) and refreshes the
    /// resolved version for the setup guide.
    func setPiExecutable(_ url: URL) async throws {
        _ = try await runPix(arguments: ["pi", "set", url.path])
        if let output = try? await runPix(arguments: ["status"]) {
            piVersion = parseVersion(from: output)
        }
    }

    /// Re-reads `pix status` without touching the service. The setup guide
    /// uses it after choosing a Pi executable.
    func refreshStatusOnly() async throws {
        let output = try await runPix(arguments: [
            "--config",
            configPath.path,
            "status",
        ])
        isConfigured = Self.isConfiguredStatus(from: output) ?? isConfigured
        piVersion = parseVersion(from: output)
        piExecutablePath = Self.parsePiExecutable(from: output)
    }

    func updatePairingRequests(_ requests: [PairingRequest]) {
        pairingRequests = requests
    }

    func pruneExpiredPairingRequests(at date: Date = Date()) {
        pairingRequests.removeAll { $0.expiresAt <= date }
    }

    func clearRelayError() {
        relayError = nil
    }

    func stop() {
        userStopped = true
        teardownService()
        Task { @MainActor [weak self] in
            guard let self else { return }
            _ = try? await runPix(arguments: [
                "--config",
                configPath.path,
                "service",
                "stop",
            ])
        }
    }

    func addWorkspace() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = String(localized: "Authorize")
        panel.message = String(localized: "Choose a workspace Pi may use.")
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let standardizedURL = url.standardizedFileURL
        guard !workspaces.contains(where: { $0.path.standardizedFileURL == standardizedURL }) else {
            return
        }
        Task { @MainActor in
            do {
                let output = try await runPix(arguments: [
                    "--config",
                    configPath.path,
                    "workspace",
                    "add",
                    url.path,
                ])
                guard let id = parseUUID(from: output) else {
                    throw HostModelError.invalidWorkspaceResponse
                }
                let workspace = WorkspaceItem(
                    id: id,
                    name: url.lastPathComponent,
                    path: standardizedURL,
                    isAvailable: true
                )
                workspaces.append(workspace)
                workspaceSessions.append(contentsOf: await loadWorkspaceSessions(for: [workspace]))
                inventoryRevision += 1
                sendServiceCommand("refresh")
            } catch {
                status = .failed(error.localizedDescription)
            }
        }
    }

    func removeWorkspace(_ workspace: WorkspaceItem) {
        Task { @MainActor in
            do {
                _ = try await runPix(arguments: [
                    "--config",
                    configPath.path,
                    "workspace",
                    "remove",
                    workspace.id.uuidString,
                ])
                workspaces.removeAll { $0.id == workspace.id }
                workspaceSessions.removeAll { $0.workspaceID == workspace.id }
                sendServiceCommand("refresh")
            } catch {
                status = .failed(error.localizedDescription)
            }
        }
    }

    func approve(_ request: PairingRequest) {
        sendServiceCommand("approve \(request.id.uuidString)")
        pairingRequests.removeAll { $0.id == request.id }
    }

    func reject(_ request: PairingRequest) {
        sendServiceCommand("reject \(request.id.uuidString)")
        pairingRequests.removeAll { $0.id == request.id }
    }

    func revoke(_ device: PairedDevice) {
        if serviceEvents != nil {
            sendServiceCommand("revoke \(device.id)")
        } else {
            Task { @MainActor in
                do {
                    _ = try await runPix(arguments: [
                        "--config",
                        configPath.path,
                        "device",
                        "revoke",
                        device.id,
                    ])
                    devices.removeAll { $0.id == device.id }
                } catch {
                    status = .failed(error.localizedDescription)
                }
            }
        }
    }

    func release(_ session: ActiveSession) {
        sendServiceCommand("release \(session.id)")
    }

    func refreshSessions() {
        sendServiceCommand("sessions")
    }

    /// Asks the host service for a fresh two-minute remote pairing channel.
    /// The QR payload arrives as a `remote_pairing_ready` event.
    func startRemotePairing() {
        guard hasActiveRelay else {
            relayError = String(localized: "Configure and enable a relay before using remote pairing.")
            return
        }
        remotePairing = nil
        remotePairingError = nil
        isRequestingRemotePairing = true
        sendServiceCommand("pair-remote")
    }

    func dismissRemotePairing() {
        remotePairing = nil
        remotePairingError = nil
        isRequestingRemotePairing = false
    }

    /// Persists a relay endpoint through the Pix CLI, then restarts the
    /// platform-managed service so its relay manager uses the new endpoint.
    func configureRelay(_ value: String) {
        relayError = nil
        guard let url = Self.normalizedRelayURL(value) else {
            relayError = String(localized: "Enter a relay URL that starts with ws:// or wss://.")
            return
        }
        updateRelay(arguments: ["relay", "set", url])
    }

    func disableRelay() {
        guard relayConfiguration.isConfigured else { return }
        updateRelay(arguments: ["relay", "disable"])
    }

    func enableRelay() {
        guard relayConfiguration.isConfigured else { return }
        updateRelay(arguments: ["relay", "enable"])
    }

    func clearRelay() {
        guard relayConfiguration.isConfigured else { return }
        updateRelay(arguments: ["relay", "clear"])
    }

    func selectPiExecutable() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.prompt = String(localized: "Use This Pi")
        panel.message = String(localized: "Choose the Pi executable Pix should start for authorized workspaces.")
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task { @MainActor in
            do {
                _ = try await runPix(arguments: [
                    "--config",
                    configPath.path,
                    "pi",
                    "set",
                    url.path,
                ])
                refresh()
            } catch {
                status = .failed(error.localizedDescription)
            }
        }
    }

    func clearPiExecutable() {
        Task { @MainActor in
            do {
                _ = try await runPix(arguments: [
                    "--config",
                    configPath.path,
                    "pi",
                    "clear",
                ])
                refresh()
            } catch {
                status = .failed(error.localizedDescription)
            }
        }
    }

    func setLaunchAtLogin(_ enabled: Bool) {
        do {
            if enabled {
                try SMAppService.mainApp.register()
            } else if SMAppService.mainApp.status == .enabled {
                try SMAppService.mainApp.unregister()
            }
            launchAtLoginEnabled = SMAppService.mainApp.status == .enabled
        } catch {
            status = .failed(error.localizedDescription)
            launchAtLoginEnabled = SMAppService.mainApp.status == .enabled
        }
    }

    func refresh() {
        guard lifecycleTask == nil else {
            refreshQueued = true
            return
        }
        beginRefresh()
    }

    private func beginRefresh() {
        userStopped = true
        teardownService()
        lifecycleTask = Task { @MainActor [weak self] in
            guard let self else { return }
            _ = try? await runPix(arguments: [
                "--config",
                configPath.path,
                "service",
                "stop",
            ])
            resolvedPixExecutable = nil
            didStart = false
            lifecycleTask = nil
            start()
        }
    }

    private func startHostService() async throws {
        guard serviceEvents == nil else { return }
        userStopped = false

        _ = try await runPix(arguments: [
            "--config",
            configPath.path,
            "service",
            "install",
            "--adopt",
        ])

        let deadline = Date().addingTimeInterval(8)
        var lastError: Error?
        while Date() < deadline {
            do {
                let connection = try UnixSocketConnection(path: eventSocketPath)
                serviceEvents = connection
                connection.handle.readabilityHandler = { [weak self] handle in
                    let data = handle.availableData
                    Task { @MainActor [weak self] in
                        guard let self else { return }
                        if data.isEmpty {
                            self.eventStreamClosed()
                        } else {
                            self.consumeServiceData(data)
                        }
                    }
                }
                // Event sockets are intentionally history-free. Reconcile
                // requests that arrived before this subscriber attached.
                sendServiceCommand("devices")
                sendServiceCommand("sessions")
                sendServiceCommand("pending")
                return
            } catch {
                lastError = error
                try await Task.sleep(for: .milliseconds(100))
            }
        }

        throw HostModelError.commandFailed(
            "Pix host service did not become ready: \(lastError?.localizedDescription ?? "event socket unavailable")"
        )
    }

    private func teardownService() {
        serviceEvents?.handle.readabilityHandler = nil
        try? serviceEvents?.handle.close()
        serviceEvents = nil
        serviceBuffer.removeAll(keepingCapacity: false)
    }

    private func eventStreamClosed() {
        guard serviceEvents != nil else { return }
        teardownService()
        guard !userStopped else { return }
        lastDiagnostic = String(localized: "Pix host service stopped unexpectedly.")
        status = .failed(lastDiagnostic ?? "")
        restartHostServiceSoon()
    }

    /// Keeps restarting the host service with capped backoff until the user
    /// quits. One crash — or one failed restart — must not leave the host
    /// permanently unreachable for paired phones.
    private func restartHostServiceSoon() {
        let delay = min(30.0, pow(2.0, Double(min(restartAttempts, 5))))
        restartAttempts += 1
        Task { @MainActor in
            try await Task.sleep(for: .seconds(delay))
            guard !userStopped else { return }
            if lifecycleTask != nil {
                refreshQueued = true
                return
            }
            guard serviceEvents == nil else { return }
            status = .starting
            do {
                try await startHostService()
            } catch {
                status = .failed(error.localizedDescription)
                restartHostServiceSoon()
            }
        }
    }

    private func sendServiceCommand(_ command: String) {
        let path = controlSocketPath
        DispatchQueue.global(qos: .utility).async { [weak self] in
            do {
                try UnixSocketConnection(path: path).sendLine(command)
            } catch {
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    self.status = .failed(String(localized: "Pix could not send the host command."))
                    if command == "pair-remote" {
                        self.remotePairingError = String(localized: "Pix could not start remote pairing.")
                        self.isRequestingRemotePairing = false
                    }
                }
            }
        }
    }

    private var controlSocketPath: URL {
        configPath
            .deletingLastPathComponent()
            .appendingPathComponent("run/host-service.sock")
    }

    private var eventSocketPath: URL {
        configPath
            .deletingLastPathComponent()
            .appendingPathComponent("run/host-events.sock")
    }

    private func consumeServiceData(_ data: Data) {
        serviceBuffer.append(data)
        while let newline = serviceBuffer.firstIndex(of: 0x0A) {
            let line = serviceBuffer[..<newline]
            serviceBuffer.removeSubrange(...newline)
            guard !line.isEmpty, let event = try? JSONDecoder().decode(ServiceEvent.self, from: Data(line)) else {
                continue
            }
            apply(event)
        }
    }

    func apply(_ event: ServiceEvent) {
        switch event {
        case .ready:
            status = .ready
            lastDiagnostic = nil
            restartAttempts = 0
            sendServiceCommand("devices")
            sendServiceCommand("sessions")
            sendServiceCommand("pending")
        case .pairingRequested(let request):
            pairingRequests.removeAll { $0.id == request.id }
            pairingRequests.append(request.value)
        case .connectionEstablished:
            status = .ready
            sendServiceCommand("devices")
            sendServiceCommand("sessions")
        case .connectionClosed:
            sendServiceCommand("sessions")
        case .connectionFailed:
            break
        case .commandError(let message):
            lastDiagnostic = message
            if isRequestingRemotePairing {
                remotePairingError = message
                isRequestingRemotePairing = false
            }
        case .deviceList(let devices):
            // The menu bar renders a presentation snapshot keyed to this
            // revision. Socket-driven inventory changes must advance it too,
            // or a phone paired after launch never appears in the menu.
            self.devices = devices
            inventoryRevision += 1
        case .deviceRevoked(let id):
            devices.removeAll { $0.id == id }
            inventoryRevision += 1
        case .sessionList(let sessions):
            self.sessions = sessions
            inventoryRevision += 1
        case .sessionReleased(let id):
            sessions.removeAll { $0.id == id }
            inventoryRevision += 1
        case .relayConfigured(let url):
            relayConfiguration = RelayConfiguration(url: url, isEnabled: true)
        case .relayChannel:
            // Standing-channel lifecycle stays payload-free; the menu needs
            // no per-device relay indicator in v1.
            break
        case .remotePairingReady(let offer):
            remotePairing = offer
            remotePairingError = nil
            isRequestingRemotePairing = false
        }
    }

    private func loadHostInventory() async {
        if let workspaceOutput = try? await runPix(arguments: [
            "--config",
            configPath.path,
            "workspace",
            "list",
        ]) {
            workspaces = parseWorkspaces(from: workspaceOutput)
            workspaceSessions = await loadWorkspaceSessions(for: workspaces)
        }
        if let deviceOutput = try? await runPix(arguments: [
            "--config",
            configPath.path,
            "device",
            "list",
        ]) {
            devices = parseDevices(from: deviceOutput)
        }
        if let piOutput = try? await runPix(arguments: [
            "--config",
            configPath.path,
            "pi",
            "show",
        ]) {
            piExecutable = parsePiExecutable(from: piOutput)
        }
        if let relayOutput = try? await runPix(arguments: [
            "--config",
            configPath.path,
            "relay",
            "show",
        ]) {
            relayConfiguration = Self.parseRelayConfiguration(from: relayOutput)
        }
        inventoryRevision += 1
    }

    private func loadWorkspaceSessions(for workspaces: [WorkspaceItem]) async -> [WorkspaceSession] {
        var result: [WorkspaceSession] = []
        for workspace in workspaces {
            guard let output = try? await runPix(arguments: [
                "--config",
                configPath.path,
                "workspace",
                "sessions",
                workspace.id.uuidString,
            ]) else {
                continue
            }
            result.append(contentsOf: Self.parseWorkspaceSessions(from: output, workspaceID: workspace.id))
        }
        return result
    }

    private func updateRelay(arguments: [String]) {
        relayError = nil
        dismissRemotePairing()
        isUpdatingRelay = true
        Task { @MainActor in
            do {
                _ = try await runPix(arguments: [
                    "--config",
                    configPath.path,
                ] + arguments)
                refresh()
            } catch {
                isUpdatingRelay = false
                relayError = error.localizedDescription
            }
        }
    }

    private func runPix(arguments: [String]) async throws -> String {
        let executable: URL
        if let resolvedPixExecutable {
            executable = resolvedPixExecutable
        } else {
            executable = try await Task.detached(priority: .utility) {
                try Self.resolvePixExecutable()
            }.value
            resolvedPixExecutable = executable
        }
        let environment = Self.hostProcessEnvironment()
        return try await Task.detached(priority: .utility) {
            let process = Process()
            let output = Pipe()
            let errors = Pipe()
            process.executableURL = executable
            process.arguments = Self.headlessArguments(arguments)
            process.environment = environment
            process.standardOutput = output
            process.standardError = errors
            try process.run()
            process.waitUntilExit()
            let stdout = output.fileHandleForReading.readDataToEndOfFile()
            let stderr = errors.fileHandleForReading.readDataToEndOfFile()
            guard process.terminationStatus == 0 else {
                let rawMessage = String(data: stderr, encoding: .utf8) ?? "Pix Core failed"
                let message = Self.parseCLIError(from: rawMessage) ?? rawMessage
                throw HostModelError.commandFailed(message.trimmingCharacters(in: .whitespacesAndNewlines))
            }
            return String(data: stdout, encoding: .utf8) ?? ""
        }.value
    }

    private func parseVersion(from output: String) -> String? {
        if let report = Self.decodeCLIData(CLIStatusData.self, from: output) {
            return report.pi.version ?? nil
        }
        return output
            .split(separator: "\n")
            .first(where: { $0.contains("pi version:") })
            .map { $0.replacingOccurrences(of: "pi version:", with: "").trimmingCharacters(in: .whitespaces) }
    }

    private func parseUUID(from output: String) -> UUID? {
        if let mutation = Self.decodeCLIData(CLIWorkspaceMutationData.self, from: output) {
            return mutation.workspace.id
        }
        return output
            .split(whereSeparator: { $0 == " " || $0 == "\n" || $0 == "(" || $0 == ")" })
            .compactMap { UUID(uuidString: String($0)) }
            .first
    }

    private func parseWorkspaces(from output: String) -> [WorkspaceItem] {
        if let inventory = Self.decodeCLIData(CLIWorkspaceListData.self, from: output) {
            return inventory.workspaces.map { workspace in
                let path = URL(fileURLWithPath: workspace.path)
                return WorkspaceItem(
                    id: workspace.id,
                    name: workspace.name,
                    path: path,
                    isAvailable: FileManager.default.fileExists(atPath: workspace.path)
                )
            }
        }
        var result: [WorkspaceItem] = []
        var pending: (UUID, String)?
        for line in output.split(separator: "\n", omittingEmptySubsequences: true) {
            if !line.hasPrefix(" "), let separator = line.firstIndex(of: " ") {
                let idText = line[..<separator]
                let name = line[line.index(after: separator)...].trimmingCharacters(in: .whitespaces)
                if let id = UUID(uuidString: String(idText)) {
                    pending = (id, name)
                }
            } else if let current = pending {
                let path = String(line).trimmingCharacters(in: .whitespaces)
                result.append(
                    WorkspaceItem(
                        id: current.0,
                        name: current.1,
                        path: URL(fileURLWithPath: path),
                        isAvailable: FileManager.default.fileExists(atPath: path)
                    )
                )
                pending = nil
            }
        }
        return result
    }

    private func parseDevices(from output: String) -> [PairedDevice] {
        guard let inventory = Self.decodeCLIData(CLIDeviceListData.self, from: output) else {
            return HostTextInventory.devices(from: output)
        }
        return inventory.devices.map { device in
            PairedDevice(
                id: device.id,
                name: device.name,
                pairedAt: ISO8601DateFormatter().date(from: device.pairedAt) ?? .distantPast
            )
        }
    }

    nonisolated static func parseWorkspaceSessions(
        from output: String,
        workspaceID: UUID
    ) -> [WorkspaceSession] {
        let decoder = JSONDecoder()
        let fractionalFormatter = ISO8601DateFormatter()
        fractionalFormatter.formatOptions.insert(.withFractionalSeconds)
        let standardFormatter = ISO8601DateFormatter()

        func session(from record: WorkspaceSessionRecord) -> WorkspaceSession? {
            guard let modifiedAt = fractionalFormatter.date(from: record.modifiedAt)
                ?? standardFormatter.date(from: record.modifiedAt)
            else { return nil }
            return WorkspaceSession(
                id: record.id,
                workspaceID: workspaceID,
                title: record.title,
                modifiedAt: modifiedAt,
                messageCount: record.messageCount
            )
        }

        // Versioned envelope first; one-record-per-line output is the legacy
        // format produced by CLIs before the machine-readable contract.
        if let data = decodeCLIData(CLIWorkspaceSessionsData.self, from: output) {
            return data.sessions.compactMap { session(from: $0) }
        }
        return output
            .split(separator: "\n", omittingEmptySubsequences: true)
            .compactMap { line in
                let data = Data(line.utf8)
                guard let record = try? decoder.decode(WorkspaceSessionRecord.self, from: data)
                else {
                    return nil
                }
                return session(from: record)
            }
    }

    /// Resolves the Pix CLI in the same order a user expects from a terminal:
    /// an explicit development override, the release bundle, the inherited
    /// PATH, the interactive login-shell PATH, and common user install paths.
    nonisolated static func resolvePixExecutable() throws -> URL {
        try resolvePixExecutable(
            environment: ProcessInfo.processInfo.environment,
            homeDirectory: FileManager.default.homeDirectoryForCurrentUser,
            bundle: .main
        )
    }

    /// Injectable resolver used by tests and by the no-argument production
    /// wrapper above. `bundle` is optional so tests can explicitly skip bundle
    /// lookup without mutating the process environment.
    nonisolated static func resolvePixExecutable(
        environment: [String: String],
        homeDirectory: URL,
        bundle: Bundle? = .main,
        searchLoginShell: Bool = true
    ) throws -> URL {
#if DEBUG
        if let override = environment["PIX_CLI"], !override.isEmpty {
            let url = URL(fileURLWithPath: override)
            guard FileManager.default.isExecutableFile(atPath: url.path) else {
                throw HostModelError.commandFailed("PIX_CLI is not an executable: \(override)")
            }
            return url
        }
#endif
        if let bundle, let bundled = bundledPixExecutable(bundle: bundle) {
            return bundled
        }

        if let found = executable(named: "pix", inPath: environment["PATH"]) {
            return found
        }
        if searchLoginShell,
           let found = executableInLoginShell(named: "pix", environment: environment) {
            return found
        }

        let candidates: [URL] = [
            homeDirectory.appendingPathComponent(".cargo/bin/pix"),
            homeDirectory.appendingPathComponent(".local/share/mise/shims/pix"),
            homeDirectory.appendingPathComponent(".local/bin/pix"),
            URL(fileURLWithPath: "/opt/homebrew/bin/pix"),
            URL(fileURLWithPath: "/usr/local/bin/pix"),
        ]
        if let found = candidates.first(where: { FileManager.default.isExecutableFile(atPath: $0.path) }) {
            return found
        }
        throw HostModelError.commandFailed(
            "pix was not found in the Pix app bundle, PATH, or login shell PATH. Set PIX_CLI for a development override, then launch Pix again."
        )
    }

    private nonisolated static func executable(named name: String, inPath path: String?) -> URL? {
        guard let path else { return nil }
        return path
            .split(separator: ":")
            .filter { !$0.isEmpty }
            .map { URL(fileURLWithPath: String($0)).appendingPathComponent(name) }
            .first(where: { FileManager.default.isExecutableFile(atPath: $0.path) })
    }

    /// Captures only a user's login-shell PATH. Shell profiles can export
    /// credentials and other sensitive values, so none of that environment is
    /// imported into Pix or written to diagnostics.
    private nonisolated static func executableInLoginShell(
        named name: String,
        environment: [String: String]
    ) -> URL? {
        var shells: [(URL, [String])] = []
        if let shell = environment["SHELL"], !shell.isEmpty {
            let shellURL = URL(fileURLWithPath: shell)
            let shellName = shellURL.lastPathComponent
            let arguments = ["zsh", "bash", "fish"].contains(shellName)
                ? ["-i", "-l", "-c"]
                : ["-l", "-c"]
            shells.append((shellURL, arguments))
        }
        for fallback in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
            let shellURL = URL(fileURLWithPath: fallback)
            guard !shells.contains(where: { $0.0 == shellURL }) else { continue }
            let arguments = shellURL.lastPathComponent == "sh"
                ? ["-l", "-c"]
                : ["-i", "-l", "-c"]
            shells.append((shellURL, arguments))
        }

        for (shell, arguments) in shells {
            guard FileManager.default.isExecutableFile(atPath: shell.path) else { continue }
            if let path = captureShellPath(shell: shell, arguments: arguments, environment: environment),
               let found = executable(named: name, inPath: path) {
                return found
            }
        }
        return nil
    }

    private nonisolated static func captureShellPath(
        shell: URL,
        arguments: [String],
        environment: [String: String]
    ) -> String? {
        let startMarker = "__PIX_PATH_START__"
        let endMarker = "__PIX_PATH_END__"
        let script = "printf '%s\\n' '\(startMarker)'; printf '%s\\n' \"$PATH\"; printf '%s\\n' '\(endMarker)'"
        let process = Process()
        let output = Pipe()
        process.executableURL = shell
        process.arguments = arguments + [script]
        process.environment = environment
        process.standardInput = FileHandle(forReadingAtPath: "/dev/null")
        process.standardOutput = output
        process.standardError = FileHandle(forWritingAtPath: "/dev/null")

        do {
            try process.run()
        } catch {
            return nil
        }

        let deadline = Date().addingTimeInterval(3)
        while process.isRunning, Date() < deadline {
            Thread.sleep(forTimeInterval: 0.025)
        }
        if process.isRunning {
            process.terminate()
            process.waitUntilExit()
            return nil
        }

        let data = output.fileHandleForReading.readDataToEndOfFile()
        guard let text = String(data: data, encoding: .utf8),
              let start = text.range(of: startMarker),
              let end = text.range(of: endMarker, range: start.upperBound..<text.endIndex)
        else {
            return nil
        }
        let path = text[start.upperBound..<end.lowerBound]
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return path.isEmpty ? nil : path
    }

    nonisolated static func bundledPixExecutable(bundle: Bundle = .main) -> URL? {
        guard let url = bundle.url(forResource: "pix", withExtension: nil),
              FileManager.default.isExecutableFile(atPath: url.path)
        else {
            return nil
        }
        return url
    }

    nonisolated static func parsePiExecutable(from output: String) -> String? {
        guard let data = output.data(using: .utf8),
              let envelope = try? JSONDecoder().decode(CLIEnvelope<CLIStatusData>.self, from: data),
              envelope.ok
        else { return nil }
        return envelope.data.pi.executable ?? nil
    }

    /// True when the CLI reports an existing host configuration. Nil when
    /// the output is not a recognizable status envelope.
    nonisolated static func isConfiguredStatus(from output: String) -> Bool? {
        guard let data = output.data(using: .utf8),
              let envelope = try? JSONDecoder().decode(CLIEnvelope<CLIStatusData>.self, from: data),
              envelope.ok
        else { return nil }
        return envelope.data.configState != "missing"
    }

    nonisolated static func headlessArguments(_ arguments: [String]) -> [String] {
        ["--output", "json", "--no-input"] + arguments
    }

    nonisolated static func hostProcessEnvironment() -> [String: String] {
        var environment = ProcessInfo.processInfo.environment
        let home = FileManager.default.homeDirectoryForCurrentUser
        let extras = [
            home.appendingPathComponent(".cargo/bin").path,
            home.appendingPathComponent(".local/share/mise/shims").path,
            home.appendingPathComponent(".local/bin").path,
            "/opt/homebrew/bin",
            "/usr/local/bin",
        ]
        let current = environment["PATH"] ?? "/usr/bin:/bin:/usr/sbin:/sbin"
        environment["PATH"] = (extras + [current]).joined(separator: ":")
        return environment
    }

    /// Pix's current host CLI uses `~/.config/pix`, while older macOS builds
    /// used `~/Library/Application Support/Pix`. Reuse an existing legacy
    /// file when present, but make the unified path the default for new users.
    nonisolated static func defaultConfigPath() -> URL {
        defaultConfigPath(homeDirectory: FileManager.default.homeDirectoryForCurrentUser)
    }

    nonisolated static func defaultConfigPath(homeDirectory: URL) -> URL {
        let current = homeDirectory
            .appendingPathComponent(".config/pix/config.json")
        let legacy = homeDirectory
            .appendingPathComponent("Library/Application Support/Pix/config.json")
        if FileManager.default.fileExists(atPath: current.path)
            || !FileManager.default.fileExists(atPath: legacy.path) {
            return current
        }
        return legacy
    }

    /// Parses the stable, human-readable output of `pix relay show` without
    /// exposing relay channel secrets to the UI model.
    nonisolated static func parseRelayConfiguration(from output: String) -> RelayConfiguration {
        if let relay = decodeCLIData(CLIRelayData.self, from: output) {
            return RelayConfiguration(url: relay.url, isEnabled: relay.enabled)
        }
        guard let line = output
            .split(separator: "\n", omittingEmptySubsequences: true)
            .map(String.init)
            .first(where: { $0.hasPrefix("relay:") })
        else {
            return .none
        }

        let prefix = "relay: "
        guard line.hasPrefix(prefix) else { return .none }
        let value = String(line.dropFirst(prefix.count))
        if value == "not configured" {
            return .none
        }
        if value.hasSuffix(" (disabled)") {
            return RelayConfiguration(
                url: String(value.dropLast(" (disabled)".count)),
                isEnabled: false
            )
        }
        if value.hasSuffix(" (enabled)") {
            return RelayConfiguration(
                url: String(value.dropLast(" (enabled)".count)),
                isEnabled: true
            )
        }
        return RelayConfiguration(url: value, isEnabled: true)
    }

    /// Relay configuration follows the CLI's WebSocket endpoint rules. Query
    /// strings and paths remain available for self-hosted relay deployments;
    /// user-info is rejected so credentials cannot be smuggled into a saved
    /// URL.
    nonisolated static func normalizedRelayURL(_ value: String) -> String? {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let components = URLComponents(string: trimmed),
              let scheme = components.scheme?.lowercased(),
              scheme == "ws" || scheme == "wss",
              let host = components.host,
              !host.isEmpty,
              components.user == nil,
              components.password == nil,
              !trimmed.contains(where: { $0.isWhitespace })
        else {
            return nil
        }
        return trimmed
    }

    private func parsePiExecutable(from output: String) -> String? {
        if let pi = Self.decodeCLIData(CLIPiData.self, from: output) {
            return pi.executable
        }
        return output
            .split(separator: "\n")
            .first
            .map { line in
                let text = String(line)
                if let separator = text.firstIndex(of: ":") {
                    return String(text[text.index(after: separator)...]).trimmingCharacters(in: .whitespaces)
                }
                return text
            }
    }

    private nonisolated static func decodeCLIData<T: Decodable>(
        _ type: T.Type,
        from output: String
    ) -> T? {
        guard let data = output.data(using: .utf8),
              let envelope = try? JSONDecoder().decode(CLIEnvelope<T>.self, from: data),
              envelope.ok,
              envelope.schemaVersion == 1
        else {
            return nil
        }
        return envelope.data
    }

    private nonisolated static func parseCLIError(from output: String) -> String? {
        guard let data = output.data(using: .utf8),
              let envelope = try? JSONDecoder().decode(CLIErrorEnvelope.self, from: data),
              !envelope.ok
        else {
            return nil
        }
        return envelope.error.message
    }
}

private struct CLIEnvelope<Payload: Decodable>: Decodable {
    let schemaVersion: Int
    let ok: Bool
    let data: Payload

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case ok
        case data
    }
}

private struct CLIErrorEnvelope: Decodable {
    struct Body: Decodable {
        let code: String
        let message: String
    }

    let ok: Bool
    let error: Body
}

private struct CLIStatusData: Decodable {
    struct Pi: Decodable {
        let version: String?
        let executable: String?
    }

    let configState: String?
    let pi: Pi

    enum CodingKeys: String, CodingKey {
        case configState = "config_state"
        case pi
    }
}

private struct CLIWorkspaceListData: Decodable {
    struct Workspace: Decodable {
        let id: UUID
        let name: String
        let path: String
    }

    let workspaces: [Workspace]
}

private struct CLIWorkspaceMutationData: Decodable {
    let workspace: CLIWorkspaceListData.Workspace
}

private struct CLIWorkspaceSessionsData: Decodable {
    let sessions: [WorkspaceSessionRecord]
}

private struct CLIDeviceListData: Decodable {
    struct Device: Decodable {
        let id: String
        let name: String
        let pairedAt: String

        enum CodingKeys: String, CodingKey {
            case id
            case name
            case pairedAt = "paired_at"
        }
    }

    let devices: [Device]
}

private struct CLIPiData: Decodable {
    let executable: String?
}

private struct CLIRelayData: Decodable {
    let url: String?
    let enabled: Bool
}

/// Small POSIX Unix-domain socket wrapper used for the local Host service
/// control/event bridge. The socket paths are derived from the selected Pix
/// config and are mode 0600 on the Rust side.
private final class UnixSocketConnection {
    let handle: FileHandle

    init(path: URL) throws {
        let fileDescriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard fileDescriptor >= 0 else {
            throw HostModelError.commandFailed(String(cString: strerror(errno)))
        }

        do {
            var address = sockaddr_un()
            address.sun_family = sa_family_t(AF_UNIX)
            let pathBytes = Array(path.path.utf8)
            let pathCapacity = MemoryLayout.size(ofValue: address.sun_path)
            guard pathBytes.count + 1 < pathCapacity else {
                throw HostModelError.commandFailed("Pix service socket path is too long.")
            }
            withUnsafeMutableBytes(of: &address.sun_path) { buffer in
                buffer.initializeMemory(as: UInt8.self, repeating: 0)
                for (index, byte) in pathBytes.enumerated() {
                    buffer[index] = byte
                }
            }

            let length = socklen_t(
                MemoryLayout<sa_family_t>.size + pathBytes.count + 1
            )
            #if os(macOS)
            address.sun_len = UInt8(length)
            #endif
            let result = withUnsafePointer(to: &address) { pointer in
                pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    Darwin.connect(fileDescriptor, $0, length)
                }
            }
            guard result == 0 else {
                throw HostModelError.commandFailed(String(cString: strerror(errno)))
            }
            handle = FileHandle(fileDescriptor: fileDescriptor, closeOnDealloc: true)
        } catch {
            Darwin.close(fileDescriptor)
            throw error
        }
    }

    func sendLine(_ command: String) throws {
        guard let data = "\(command)\n".data(using: .utf8) else {
            throw HostModelError.commandFailed("Pix service command is not valid UTF-8.")
        }
        try handle.write(contentsOf: data)
        _ = Darwin.shutdown(handle.fileDescriptor, SHUT_WR)
        let response = try handle.read(upToCount: 64) ?? Data()
        guard String(data: response, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) == "ok" else {
            throw HostModelError.commandFailed("Pix host service rejected the command.")
        }
    }
}

private enum HostModelError: LocalizedError {
    case commandFailed(String)
    case invalidWorkspaceResponse

    var errorDescription: String? {
        switch self {
        case .commandFailed(let message): message
        case .invalidWorkspaceResponse: String(localized: "Pix returned an invalid workspace response.")
        }
    }
}

enum ServiceEvent: Decodable {
    case ready
    case pairingRequested(PairingRequestEvent)
    case connectionEstablished
    case connectionClosed
    case connectionFailed(String)
    case commandError(String)
    case deviceList([PairedDevice])
    case deviceRevoked(String)
    case sessionList([ActiveSession])
    case sessionReleased(String)
    case relayConfigured(String)
    case relayChannel(label: String, state: String)
    case remotePairingReady(RemotePairingOffer)

    private enum CodingKeys: String, CodingKey {
        case type
        case id
        case deviceName = "device_name"
        case confirmationCode = "confirmation_code"
        case expiresAt = "expires_at"
        case stage
        case message
        case devices
        case deviceID = "device_id"
        case sessions
        case sessionID = "session_id"
        case url
        case label
        case state
        case qrPayload = "qr_payload"
        case joinCode = "join_code"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(String.self, forKey: .type) {
        case "ready": self = .ready
        case "pairing_requested":
            self = .pairingRequested(
                PairingRequestEvent(
                    id: try container.decode(UUID.self, forKey: .id),
                    deviceName: try container.decode(String.self, forKey: .deviceName),
                    confirmationCode: try container.decode(String.self, forKey: .confirmationCode),
                    expiresAt: try container.decode(TimeInterval.self, forKey: .expiresAt)
                )
            )
        case "connection_established": self = .connectionEstablished
        case "connection_closed": self = .connectionClosed
        case "connection_failed":
            self = .connectionFailed(try container.decode(String.self, forKey: .stage))
        case "command_error":
            self = .commandError(try container.decode(String.self, forKey: .message))
        case "device_list":
            self = .deviceList(try container.decode([ServiceDevice].self, forKey: .devices).map(\.value))
        case "device_revoked":
            self = .deviceRevoked(try container.decode(String.self, forKey: .deviceID))
        case "session_list":
            self = .sessionList(try container.decode([ServiceSession].self, forKey: .sessions).map(\.value))
        case "session_released":
            self = .sessionReleased(try container.decode(String.self, forKey: .sessionID))
        case "relay_configured":
            self = .relayConfigured(try container.decode(String.self, forKey: .url))
        case "relay_channel":
            self = .relayChannel(
                label: try container.decode(String.self, forKey: .label),
                state: try container.decode(String.self, forKey: .state)
            )
        case "remote_pairing_ready":
            self = .remotePairingReady(
                RemotePairingOffer(
                    qrPayload: try container.decode(String.self, forKey: .qrPayload),
                    joinCode: try container.decodeIfPresent(String.self, forKey: .joinCode) ?? "",
                    expiresAt: Date(
                        timeIntervalSince1970: try container.decode(TimeInterval.self, forKey: .expiresAt)
                    )
                )
            )
        default:
            throw DecodingError.dataCorruptedError(forKey: .type, in: container, debugDescription: "Unknown Pix host event")
        }
    }
}

private struct ServiceDevice: Decodable {
    let id: String
    let name: String
    let pairedAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case pairedAt = "paired_at"
    }

    var value: PairedDevice {
        PairedDevice(
            id: id,
            name: name,
            pairedAt: ISO8601DateFormatter().date(from: pairedAt) ?? .distantPast
        )
    }
}

private struct ServiceSession: Decodable {
    let id: String
    let workspace: String
    let clients: Int
    let state: String

    var value: ActiveSession {
        ActiveSession(id: id, workspacePath: workspace, clients: clients, state: state)
    }
}

private struct WorkspaceSessionRecord: Decodable {
    let id: String
    let title: String?
    let modifiedAt: String
    let messageCount: Int

    enum CodingKeys: String, CodingKey {
        case id
        case title
        case modifiedAt = "modified_at"
        case messageCount = "message_count"
    }
}

struct PairingRequestEvent {
    let id: UUID
    let deviceName: String
    let confirmationCode: String
    let expiresAt: TimeInterval

    var value: PairingRequest {
        PairingRequest(
            id: id,
            deviceName: deviceName,
            confirmationCode: confirmationCode,
            expiresAt: Date(timeIntervalSince1970: expiresAt)
        )
    }
}
