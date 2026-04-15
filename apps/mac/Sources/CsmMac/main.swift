import AppKit

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
// Hide from the dock. Works for `swift run`; the bundled .app also sets
// LSUIElement=YES in Info.plist so the activation policy is consistent.
app.setActivationPolicy(.accessory)
app.run()
