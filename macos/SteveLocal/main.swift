import AppKit
import Foundation

func clientSettings(model: String) -> String {
    "Base URL: http://127.0.0.1:11435/v1\nModel: \(model)\nClient API key: local (nonsecret placeholder)"
}

func accountingSummary(_ status: [String: Any]) -> String {
    let incident = status["accounting_incident"] as? [String: Any] ?? [:]
    let queues = status["queues"] as? [String: Any] ?? [:]
    return "Accounting: \(incident["state"] as? String ?? "unknown")\nLost: \(queues["accounting_lost"] as? Int ?? 0)   Spilled: \(queues["accounting_spilled"] as? Int ?? 0)\nActive inference requests: \(status["active_inference_requests"] as? Int ?? 0)"
}

func daemonIsPresent(_ status: [String: Any]?) -> Bool {
    status?["name"] as? String == "steve"
}

func daemonIsReady(_ status: [String: Any]?, _ readiness: [String: Any]?) -> Bool {
    daemonIsPresent(status) && readiness?["status"] as? String == "ready"
}

if CommandLine.arguments.contains("--self-test") {
    assert(clientSettings(model: "luna").contains("Model: luna"))
    assert(clientSettings(model: "auto").contains("Model: auto"))
    assert(!clientSettings(model: "luna").contains("OPENAI_API_KEY"))
    assert(accountingSummary(["accounting_incident": ["state": "blocked"], "queues": ["accounting_lost": 2]]).contains("Lost: 2"))
    assert(accountingSummary([:]).contains("unknown"))
    assert(daemonIsPresent(["name": "steve", "phase": "draining"]))
    assert(daemonIsPresent(["name": "steve", "phase": "starting"]))
    assert(!daemonIsPresent(nil))
    assert(!daemonIsReady(["name": "steve", "phase": "ready"], nil))
    assert(daemonIsReady(["name": "steve"], ["status": "ready"]))
    print("native controller self-checks passed")
    exit(0)
}

final class Controller: NSObject, NSApplicationDelegate {
    private var window: NSWindow!
    private let connection = NSTextField(labelWithString: "Checking Steve…")
    private let accounting = NSTextField(labelWithString: "Accounting: unknown")
    private let message = NSTextField(wrappingLabelWithString: "")
    private let models = NSPopUpButton(frame: .zero, pullsDown: false)
    private var startButton: NSButton!
    private var stopButton: NSButton!
    private var child: Process?
    private var timer: Timer?
    private var refreshing = false
    private var daemonPresent = false
    private var quitting = false
    private var connected = false
    private var appStarted = false
    private let configPath = Bundle.main.object(forInfoDictionaryKey: "SteveConfigPath") as? String ?? ""
    private let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.connectionProxyDictionary = [:]
        configuration.timeoutIntervalForRequest = 3
        return URLSession(configuration: configuration)
    }()

    private func label(_ text: String) -> NSTextField {
        let label = NSTextField(wrappingLabelWithString: text)
        label.isSelectable = true
        label.preferredMaxLayoutWidth = 610
        return label
    }
    private func button(_ title: String, _ action: Selector) -> NSButton {
        NSButton(title: title, target: self, action: action)
    }
    func applicationDidFinishLaunching(_ notification: Notification) {
        let menu = NSMenu()
        let appItem = NSMenuItem()
        menu.addItem(appItem)
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "Quit Steve", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appItem.submenu = appMenu
        NSApplication.shared.mainMenu = menu
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 660, height: 720), styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        window.title = "Steve — Local Proxy"
        window.isReleasedWhenClosed = false
        let heading = label("Steve")
        heading.font = .systemFont(ofSize: 28, weight: .bold)
        connection.font = .systemFont(ofSize: 16, weight: .semibold)
        connection.setAccessibilityIdentifier("connection-status")
        accounting.setAccessibilityIdentifier("accounting-status")
        models.addItem(withTitle: "auto")
        models.setAccessibilityLabel("Client model")
        models.setAccessibilityIdentifier("client-model")
        models.target = self
        models.action = #selector(modelChanged)
        startButton = button("Start Steve", #selector(start))
        stopButton = button("Stop Steve", #selector(stop))
        let actions = NSStackView(views: [startButton, stopButton, button("Refresh", #selector(refresh))])
        actions.orientation = .horizontal
        let setup = NSStackView(views: [models, button("Copy client settings", #selector(copySettings)), button("Show configuration", #selector(showConfig))])
        setup.orientation = .horizontal
        let stack = NSStackView(views: [heading, connection,
            label("Proxy: http://127.0.0.1:11435/v1\nManagement: http://127.0.0.1:8790"), actions,
            label("Choose the model to use in your OpenAI-compatible client:"), setup,
            label("Models come from your private configuration. auto chooses the lowest configured eligible cost; ties use public model ID. Edit the private configuration and restart to change routes or prices."),
            accounting, label("Setup: \(configPath)"),
            label("Local OS users can access this loopback proxy. Client key ‘local’ is a placeholder; Steve substitutes the server credential. Requests you send can incur provider charges."), message])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 13
        stack.translatesAutoresizingMaskIntoConstraints = false
        window.contentView!.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: window.contentView!.leadingAnchor, constant: 24),
            stack.trailingAnchor.constraint(equalTo: window.contentView!.trailingAnchor, constant: -24),
            stack.topAnchor.constraint(equalTo: window.contentView!.topAnchor, constant: 20)])
        window.minSize = NSSize(width: 660, height: 720)
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApplication.shared.activate(ignoringOtherApps: true)
        timer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in self?.refresh() }
        refresh()
    }

    private func get(_ url: String, completion: @escaping ([String: Any]?) -> Void) {
        session.dataTask(with: URL(string: url)!) { data, response, _ in
            let value: [String: Any]?
            if (response as? HTTPURLResponse)?.statusCode == 200, let data {
                value = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
            } else { value = nil }
            DispatchQueue.main.async { completion(value) }
        }.resume()
    }
    @objc private func refresh() {
        guard !refreshing else { return }
        refreshing = true
        get("http://127.0.0.1:8790/api/v1/system/status") { [weak self] status in
            guard let self else { return }
            self.daemonPresent = daemonIsPresent(status)
            self.accounting.stringValue = status.map(accountingSummary) ?? "Accounting: unavailable while disconnected"
            self.startButton.isEnabled = !self.daemonPresent && self.child == nil
            self.stopButton.isEnabled = self.child?.isRunning == true
            if !self.appStarted {
                self.appStarted = true
                if !self.daemonPresent { self.start() }
                else { self.message.stringValue = "Connected to an existing daemon. Start/stop ownership stays with its original launcher." }
            }
            guard self.daemonPresent else {
                self.connected = false
                self.connection.stringValue = "Disconnected — start Steve to use the proxy"
                self.refreshing = false
                return
            }
            self.get("http://127.0.0.1:11435/health/ready") { [weak self] readiness in
                guard let self else { return }
                self.connected = daemonIsReady(status, readiness)
                let phase = status?["phase"] as? String ?? "unknown"
                self.connection.stringValue = self.connected ? "Ready — local proxy connected" : "Not ready — \(phase); check accounting below"
                if self.connected && self.message.stringValue.hasPrefix("Starting app-managed") {
                    self.message.stringValue = "App-managed Steve is running. Closing this window keeps it running; Quit Steve stops it."
                }
                self.get("http://127.0.0.1:11435/v1/models") { [weak self] catalogue in
                    guard let self else { return }
                    let selected = self.models.titleOfSelectedItem ?? "auto"
                    let ids = ((catalogue?["data"] as? [[String: Any]]) ?? []).compactMap { $0["id"] as? String }
                    self.models.removeAllItems()
                    self.models.addItems(withTitles: ["auto"] + ids)
                    self.models.selectItem(withTitle: selected)
                    if ids.isEmpty { self.message.stringValue = "Model catalogue unavailable. Check setup before sending requests." }
                    self.refreshing = false
                }
            }
        }
    }
    @objc private func start() {
        guard child == nil, !daemonPresent else { return }
        guard !(ProcessInfo.processInfo.environment["OPENAI_API_KEY"] ?? "").isEmpty else {
            message.stringValue = "No credential is available to this app process. Use the approved environment launcher; Steve never saves the key."
            return
        }
        guard let binary = Bundle.main.url(forResource: "steve", withExtension: nil), FileManager.default.fileExists(atPath: configPath) else {
            message.stringValue = "Missing bundled daemon or private configuration. Rebuild the app with the prepared configuration path."
            return
        }
        let process = Process()
        process.executableURL = binary
        process.arguments = ["--config", configPath, "serve"]
        process.environment = ProcessInfo.processInfo.environment.filter { !["http_proxy", "https_proxy", "all_proxy", "rust_log"].contains($0.key.lowercased()) }
        // Existing daemon logs contain metadata only; keep them next to private configuration.
        let logPath = URL(fileURLWithPath: configPath).deletingLastPathComponent().appendingPathComponent("app-daemon.log").path
        if !FileManager.default.fileExists(atPath: logPath) {
            FileManager.default.createFile(atPath: logPath, contents: nil, attributes: [.posixPermissions: 0o600])
        }
        guard let log = FileHandle(forWritingAtPath: logPath) else {
            message.stringValue = "Cannot open private daemon log."
            return
        }
        log.seekToEndOfFile()
        process.standardOutput = log
        process.standardError = log
        process.terminationHandler = { [weak self] finished in
            DispatchQueue.main.async {
                guard let self, self.child === finished else { return }
                self.child = nil
                if self.quitting { NSApplication.shared.reply(toApplicationShouldTerminate: true); return }
                self.message.stringValue = finished.terminationStatus == 0 ? "Steve stopped. Start it again when needed." : "Steve exited with status \(finished.terminationStatus). Check the private daemon log."
                self.refresh()
            }
        }
        do {
            try process.run()
            child = process
            message.stringValue = "Starting app-managed Steve. Closing this window keeps it running; Quit Steve stops it."
            startButton.isEnabled = false
        } catch { message.stringValue = "Could not launch the bundled daemon." }
    }
    @objc private func stop() {
        guard let child, child.isRunning else { return }
        message.stringValue = "Stopping Steve gracefully…"
        child.terminate()
        stopButton.isEnabled = false
    }
    @objc private func copySettings() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(clientSettings(model: models.titleOfSelectedItem ?? "auto"), forType: .string)
        message.stringValue = "Client settings copied. Paste into your OpenAI-compatible client; no provider secret is included."
    }
    @objc private func modelChanged() {
        message.stringValue = "Client model: \(models.titleOfSelectedItem ?? "auto"). Copy client settings to use this selection."
    }
    @objc private func showConfig() {
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: configPath)])
    }
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        window.makeKeyAndOrderFront(nil)
        return true
    }
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let child, child.isRunning else { return .terminateNow }
        quitting = true
        message.stringValue = "Waiting for Steve to drain before quitting…"
        child.terminate()
        return .terminateLater
    }
    func applicationWillTerminate(_ notification: Notification) {
        timer?.invalidate()
        if child?.isRunning == true { child?.terminate() }
    }
}

let app = NSApplication.shared
let controller = Controller()
app.setActivationPolicy(.regular)
app.delegate = controller
app.run()
