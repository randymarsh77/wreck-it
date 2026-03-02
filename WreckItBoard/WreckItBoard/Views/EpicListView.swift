import SwiftUI

/// List of epics with expandable sub-task hierarchies and progress bars.
struct EpicListView: View {
    @EnvironmentObject var store: ProjectStore

    var body: some View {
        List {
            if !store.epics.isEmpty {
                Section("Epics") {
                    ForEach(store.epics) { epic in
                        EpicRow(epic: epic)
                    }
                }
            }

            if !store.standaloneTasks.isEmpty {
                Section("Standalone Tasks") {
                    ForEach(store.standaloneTasks) { task in
                        TaskRow(task: task)
                    }
                }
            }

            if store.tasks.isEmpty {
                ContentUnavailableView(
                    "No Tasks",
                    systemImage: "tray",
                    description: Text("Create a task to get started.")
                )
            }
        }
        .navigationTitle("Epics & Tasks")
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

/// A row for a single epic with progress and expandable sub-tasks.
private struct EpicRow: View {
    @EnvironmentObject var store: ProjectStore
    let epic: WreckItTask
    @State private var isExpanded = true

    private var subTasks: [WreckItTask] {
        store.subTasks(of: epic.id)
    }

    private var progress: Double {
        store.epicProgress(epic.id)
    }

    var body: some View {
        DisclosureGroup(isExpanded: $isExpanded) {
            ForEach(subTasks) { sub in
                TaskRow(task: sub)
                    .padding(.leading, 16)
            }
        } label: {
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Image(systemName: "folder.fill")
                        .foregroundColor(.purple)
                    Text(epic.description)
                        .font(.headline)
                    Spacer()
                    Text("\(subTasks.count) sub-tasks")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                ProgressView(value: progress)
                    .tint(progressColor)

                HStack {
                    Text(epic.id)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Text("\(Int(progress * 100))% complete")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }

                if let labels = epic.labels, !labels.isEmpty {
                    HStack(spacing: 4) {
                        ForEach(labels, id: \.self) { label in
                            Text(label)
                                .font(.caption2)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(Color.accentColor.opacity(0.12))
                                .clipShape(Capsule())
                        }
                    }
                }
            }
            .padding(.vertical, 4)
        }
    }

    private var progressColor: Color {
        if progress >= 1.0 { return .green }
        if progress > 0 { return .blue }
        return .gray
    }
}

/// A simple row for a task (used for sub-tasks and standalone tasks).
private struct TaskRow: View {
    @EnvironmentObject var store: ProjectStore
    let task: WreckItTask
    @State private var showDetail = false

    var body: some View {
        HStack {
            Image(systemName: task.status.iconName)
                .foregroundColor(statusColor)

            VStack(alignment: .leading, spacing: 2) {
                Text(task.description)
                    .font(.body)
                HStack(spacing: 8) {
                    Text(task.id)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    if let labels = task.labels, !labels.isEmpty {
                        Text(labels.joined(separator: ", "))
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
            }

            Spacer()

            Menu {
                ForEach(TaskStatus.allCases) { status in
                    Button {
                        store.moveTask(id: task.id, to: status)
                    } label: {
                        Label(status.displayName, systemImage: status.iconName)
                    }
                    .disabled(status == task.status)
                }
                Divider()
                Button(role: .destructive) {
                    store.deleteTask(id: task.id)
                } label: {
                    Label("Delete", systemImage: "trash")
                }
            } label: {
                Image(systemName: "ellipsis.circle")
                    .foregroundStyle(.secondary)
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
        }
        .contentShape(Rectangle())
        .onTapGesture {
            showDetail = true
        }
        .sheet(isPresented: $showDetail) {
            TaskDetailView(task: task)
        }
    }

    private var statusColor: Color {
        switch task.status {
        case .pending:    return .gray
        case .inprogress: return .blue
        case .completed:  return .green
        case .failed:     return .red
        }
    }
}

#Preview {
    EpicListView()
        .environmentObject(ProjectStore.preview)
        .frame(width: 600, height: 500)
}
