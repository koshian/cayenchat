// Reports how much of a process's largest on-screen window is not covered by
// windows in front of it, as JSON: {"onscreen": bool, "visible_fraction": x}.
//
// GPUI on macOS stops drawing a window that AppKit reports as fully occluded,
// so a hidden window makes a load measurement look cheaper than it is.
// Coverage is estimated on a 40x40 grid from CGWindowList bounds (front to
// back); translucent overlays with zero alpha are ignored. Window titles are
// not read, so no Screen Recording permission is needed. With --activate the
// process is first brought to the front (its windows move to the active Space
// only if macOS allows it; the report shows the outcome).
//
//   swiftc -O scripts/perf/window_visibility.swift -o target/perf/window_visibility
//   target/perf/window_visibility [--activate] PID

import AppKit
import CoreGraphics
import Foundation

var arguments = Array(CommandLine.arguments.dropFirst())
let activate = arguments.first == "--activate"
if activate { arguments.removeFirst() }
guard arguments.count == 1, let pid = Int(arguments[0]) else {
    FileHandle.standardError.write("usage: window_visibility [--activate] PID\n".data(using: .utf8)!)
    exit(2)
}
if activate, let app = NSRunningApplication(processIdentifier: pid_t(pid)) {
    app.activate(options: [.activateAllWindows])
    Thread.sleep(forTimeInterval: 0.5)
}

let info = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
    as? [[String: Any]] ?? []

func bounds(_ window: [String: Any]) -> CGRect? {
    guard let dict = window[kCGWindowBounds as String] as? NSDictionary else { return nil }
    return CGRect(dictionaryRepresentation: dict as CFDictionary)
}

var target: (index: Int, rect: CGRect)?
for (index, window) in info.enumerated() {
    guard (window[kCGWindowOwnerPID as String] as? Int) == pid,
          (window[kCGWindowLayer as String] as? Int) == 0,
          let rect = bounds(window) else { continue }
    if target == nil || rect.width * rect.height > target!.rect.width * target!.rect.height {
        target = (index, rect)
    }
}

guard let (index, rect) = target else {
    print(#"{"onscreen": false, "visible_fraction": 0}"#)
    exit(0)
}

let covers = info[..<index].compactMap { window -> CGRect? in
    let alpha = window[kCGWindowAlpha as String] as? Double ?? 1
    return alpha > 0 ? bounds(window) : nil
}
let steps = 40
var visible = 0
for row in 0..<steps {
    for column in 0..<steps {
        let point = CGPoint(
            x: rect.minX + (CGFloat(column) + 0.5) * rect.width / CGFloat(steps),
            y: rect.minY + (CGFloat(row) + 0.5) * rect.height / CGFloat(steps))
        if !covers.contains(where: { $0.contains(point) }) { visible += 1 }
    }
}
let fraction = Double(visible) / Double(steps * steps)
print(#"{"onscreen": true, "visible_fraction": \#(String(format: "%.3f", fraction))}"#)
