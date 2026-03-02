import SwiftUI

/// Kanban-style board view with columns for each task status.
struct BoardView: View {
    @EnvironmentObject var store: ProjectStore

    var body: some View {
        ScrollView(.horizontal, showsIndicators: true) {
            HStack(alignment: .top, spacing: 16) {
                ForEach(TaskStatus.allCases) { status in
                    BoardColumn(status: status)
                }
            }
            .padding()
        }
        .navigationTitle("Board")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    store.showNewTaskSheet = true
                } label: {
                    Label("New Task", systemImage: "plus")
                }
            }
        }
    }
}

/// A single status column in the board.
private struct BoardColumn: View {
    @EnvironmentObject var store: ProjectStore
    let status: TaskStatus

    private var tasks: [WreckItTask] {
        store.tasksByStatus(status)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Column header
            HStack {
                Image(systemName: status.iconName)
                    .foregroundColor(statusColor)
                Text(status.displayName)
                    .font(.headline)
                Spacer()
                Text("\(tasks.count)")
                    .font(.caption)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 2)
                    .background(statusColor.opacity(0.15))
                    .clipShape(Capsule())
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)

            Divider()

            // Task cards
            ScrollView(.vertical, showsIndicators: false) {
                LazyVStack(spacing: 8) {
                    ForEach(tasks) { task in
                        TaskCardView(task: task)
                    }
                }
                .padding(8)
            }
        }
        .frame(width: 280)
        .background(Color(nsColor: .controlBackgroundColor))
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .shadow(color: .black.opacity(0.08), radius: 4, y: 2)
    }

    private var statusColor: Color {
        switch status {
        case .pending:    return .gray
        case .inprogress: return .blue
        case .completed:  return .green
        case .failed:     return .red
        }
    }
}

#Preview {
    BoardView()
        .environmentObject(ProjectStore.preview)
        .frame(width: 1200, height: 700)
}
