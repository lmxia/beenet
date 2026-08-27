import AppKit
import SwiftUI

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        NSApp.applicationIconImage = BeenetIcon.app
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }
}

@main
struct BeenetApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var model = ContributorModel()

    var body: some Scene {
        WindowGroup("Beenet") {
            MainView()
                .environmentObject(model)
        }
        .windowResizability(.contentSize)
        .defaultSize(width: 400, height: 620)
        .commands {
            CommandGroup(replacing: .appSettings) {
                Button("设置…") {
                    model.openSettings()
                }
                .keyboardShortcut(",", modifiers: .command)
            }
        }

        MenuBarExtra {
            MenuBarView()
                .environmentObject(model)
        } label: {
            Image(nsImage: BeenetIcon.menuBar)
                .renderingMode(.original)
                .opacity(model.isContributing ? 1 : 0.55)
                .help(model.menuTitle)
        }
    }
}
