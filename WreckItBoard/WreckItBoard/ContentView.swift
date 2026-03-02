import SwiftUI

/// Root content view with a sidebar for navigation and a detail area.
struct ContentView: View {
    @EnvironmentObject var store: ProjectStore
    @State private var selection: SidebarItem? = .board

    enum SidebarItem: Hashable {
        case board
        case epics
    }

    var body: some View {
        NavigationSplitView {
            List(selection: $selection) {
                Label("Board", systemImage: "rectangle.split.3x1")
                    .tag(SidebarItem.board)
                Label("Epics", systemImage: "list.bullet.indent")
                    .tag(SidebarItem.epics)
            }
            .navigationTitle("wreck-it")
            .listStyle(.sidebar)
            .toolbar {
                ToolbarItem {
                    Button {
                        store.reload()
                    } label: {
                        Label("Refresh", systemImage: "arrow.clockwise")
                    }
                }
            }
        } detail: {
            switch selection {
            case .board:
                BoardView()
            case .epics:
                EpicListView()
            case .none:
                Text("Select a view")
                    .foregroundStyle(.secondary)
            }
        }
        .sheet(isPresented: $store.showNewTaskSheet) {
            NewTaskSheet()
        }
        .onAppear {
            store.reload()
        }
    }
}

#Preview {
    ContentView()
        .environmentObject(ProjectStore.preview)
}
