import CoreGraphics
import Foundation
// `winfo list` prints every on-screen layer-0 window as "owner|pid|id|x|y|w|h".
// `winfo <owner>` prints "id x y w h" for that owner's largest window.
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
let mode = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "Suzuri"
var best: (Int, CGRect)? = nil
for window in list {
    guard let name = window[kCGWindowOwnerName as String] as? String,
          let layer = window[kCGWindowLayer as String] as? Int, layer == 0,
          let id = window[kCGWindowNumber as String] as? Int,
          let pid = window[kCGWindowOwnerPID as String] as? Int,
          let boundsDict = window[kCGWindowBounds as String] as? NSDictionary,
          let bounds = CGRect(dictionaryRepresentation: boundsDict) else { continue }
    if mode == "list" {
        if bounds.height > 300 { print("\(name)|\(pid)|\(id)|\(Int(bounds.origin.x))|\(Int(bounds.origin.y))|\(Int(bounds.width))|\(Int(bounds.height))") }
        continue
    }
    guard name == mode, bounds.height > 300 else { continue }
    if best == nil || bounds.width * bounds.height > best!.1.width * best!.1.height { best = (id, bounds) }
}
if mode == "list" { exit(0) }
if let (id, b) = best { print("\(id) \(Int(b.origin.x)) \(Int(b.origin.y)) \(Int(b.width)) \(Int(b.height))") } else { print("none"); exit(1) }
