#!/usr/bin/env python3
// 按 PID 列出屏幕上的窗口，供安装验收截图定位窗口编号。
// 用法: swift dist/tools/window-list.swift <pid>
// 输出: 每行 "<windowNumber> <width> <height> <x> <y> <name>"
import CoreGraphics
import Foundation

let args = CommandLine.arguments
guard args.count > 1, let pid = Int(args[1]) else {
    FileHandle.standardError.write("usage: window-list.swift <pid>\n".data(using: .utf8)!)
    exit(2)
}
let info = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
for w in info {
    guard (w[kCGWindowOwnerPID as String] as? Int) == pid else { continue }
    let num = w[kCGWindowNumber as String] as? Int ?? -1
    let bounds = w[kCGWindowBounds as String] as? [String: Any] ?? [:]
    let width = Int((bounds["Width"] as? Double) ?? 0)
    let height = Int((bounds["Height"] as? Double) ?? 0)
    let x = Int((bounds["X"] as? Double) ?? 0)
    let y = Int((bounds["Y"] as? Double) ?? 0)
    let name = w[kCGWindowName as String] as? String ?? ""
    print("\(num) \(width) \(height) \(x) \(y) \(name)")
}
