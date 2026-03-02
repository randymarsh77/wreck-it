import SwiftUI

@main
struct WreckItBoardApp: App {
    @StateObject private var store = ProjectStore()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(store)
        }
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("New Task…") {
                    store.showNewTaskSheet = true
                }
                .keyboardShortcut("n")
            }
        }
    }
}
