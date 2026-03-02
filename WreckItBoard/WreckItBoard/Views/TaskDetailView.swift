import SwiftUI

/// Detail sheet for viewing and editing a single task.
struct TaskDetailView: View {
    @EnvironmentObject var store: ProjectStore
    @Environment(\.dismiss) private var dismiss
    let task: WreckItTask

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            // Header
            HStack {
                Image(systemName: task.status.iconName)
                    .font(.title2)
                    .foregroundColor(statusColor)
                VStack(alignment: .leading) {
                    Text(task.id)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text(task.description)
                        .font(.title3)
                        .fontWeight(.semibold)
                }
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }

            Divider()

            // Metadata grid
            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 8) {
                GridRow {
                    Text("Status").foregroundStyle(.secondary)
                    Text(task.status.displayName)
                }
                if let pid = task.parentId {
                    GridRow {
                        Text("Epic").foregroundStyle(.secondary)
                        Text(pid)
                    }
                }
                GridRow {
                    Text("Phase").foregroundStyle(.secondary)
                    Text("\(task.phase ?? 1)")
                }
                GridRow {
                    Text("Priority").foregroundStyle(.secondary)
                    Text("\(task.priority ?? 0)")
                }
                GridRow {
                    Text("Complexity").foregroundStyle(.secondary)
                    Text("\(task.complexity ?? 1)")
                }
                if let deps = task.dependsOn, !deps.isEmpty {
                    GridRow {
                        Text("Depends On").foregroundStyle(.secondary)
                        Text(deps.joined(separator: ", "))
                    }
                }
            }

            // Labels
            if let labels = task.labels, !labels.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Labels").foregroundStyle(.secondary)
                    HStack(spacing: 4) {
                        ForEach(labels, id: \.self) { label in
                            Text(label)
                                .font(.caption)
                                .padding(.horizontal, 8)
                                .padding(.vertical, 4)
                                .background(Color.accentColor.opacity(0.12))
                                .clipShape(Capsule())
                        }
                    }
                }
            }

            // Sub-tasks (if this is an epic)
            let subs = store.subTasks(of: task.id)
            if !subs.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Sub-tasks (\(subs.count))")
                        .font(.headline)
                    ProgressView(value: store.epicProgress(task.id))
                        .tint(.blue)
                    ForEach(subs) { sub in
                        HStack {
                            Image(systemName: sub.status.iconName)
                                .foregroundColor(subStatusColor(sub.status))
                            Text(sub.description)
                                .font(.subheadline)
                            Spacer()
                            Text(sub.status.displayName)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
            }

            Spacer()

            // Actions
            HStack {
                ForEach(TaskStatus.allCases) { status in
                    if status != task.status {
                        Button {
                            store.moveTask(id: task.id, to: status)
                            dismiss()
                        } label: {
                            Label(status.displayName, systemImage: status.iconName)
                        }
                    }
                }
                Spacer()
                Button(role: .destructive) {
                    store.deleteTask(id: task.id)
                    dismiss()
                } label: {
                    Label("Delete", systemImage: "trash")
                }
            }
        }
        .padding(20)
        .frame(minWidth: 450, minHeight: 350)
    }

    private var statusColor: Color {
        switch task.status {
        case .pending:    return .gray
        case .inprogress: return .blue
        case .completed:  return .green
        case .failed:     return .red
        }
    }

    private func subStatusColor(_ status: TaskStatus) -> Color {
        switch status {
        case .pending:    return .gray
        case .inprogress: return .blue
        case .completed:  return .green
        case .failed:     return .red
        }
    }
}

#Preview {
    TaskDetailView(task: WreckItTask.samples[0])
        .environmentObject(ProjectStore.preview)
}
