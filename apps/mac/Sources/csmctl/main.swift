import Foundation
import CsmCore

/// `csmctl` — minimal CLI harness proving the Rust core FFI works end-to-end
/// from Swift. Subscribes via SessionObserver and prints each callback, then
/// exits after `--duration` seconds so CI runs are bounded.
///
/// Usage: csmctl [--server URL] [--duration SECONDS]

final class PrintObserver: SessionObserver {
    func onSessionsChanged(sessions: [SessionView]) {
        FileHandle.standardOutput.write(Data("sessions changed (count=\(sessions.count))\n".utf8))
        for s in sessions {
            FileHandle.standardOutput.write(Data("  - \(s.sessionId) \(s.status)\n".utf8))
        }
    }
    func onConnectionChanged(state: ConnectionState) {
        FileHandle.standardOutput.write(Data("connection changed: \(state)\n".utf8))
    }
    func onSummaryChanged(summary: MenuBarSummary) {
        FileHandle.standardOutput.write(Data(
            "summary: waiting_input=\(summary.waitingInput) waiting_permission=\(summary.waitingPermission) working=\(summary.working)\n".utf8
        ))
    }
}

func parseArgs() -> (serverUrl: String?, duration: Double) {
    var serverUrl: String? = nil
    var duration: Double = 3.0
    var it = CommandLine.arguments.dropFirst().makeIterator()
    while let arg = it.next() {
        switch arg {
        case "--server":
            serverUrl = it.next()
        case "--duration":
            if let d = it.next(), let v = Double(d) { duration = v }
        default:
            FileHandle.standardError.write(Data("unknown argument: \(arg)\n".utf8))
            exit(2)
        }
    }
    return (serverUrl, duration)
}

let (serverUrl, duration) = parseArgs()
let guardToken = initTelemetry(appLabel: "csmctl", logLevel: "info")
_ = guardToken  // keep alive via local binding

print("csmctl starting (server=\(serverUrl ?? "<default>"), duration=\(duration)s)")
let core = CoreHandle(serverUrl: serverUrl)
let observer = PrintObserver()
let subscription = core.subscribe(observer: observer)
_ = subscription  // keep alive until exit

Thread.sleep(forTimeInterval: duration)
print("csmctl done")
