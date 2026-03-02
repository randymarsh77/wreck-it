import SwiftUI

/// Sheet for creating a new task or sub-task.
struct NewTaskSheet: View {
    @EnvironmentObject var store: ProjectStore
    @Environment(\.dismiss) private var dismiss

    @State private var id = ""
    @State private var description = ""
    @State private var selectedParent: String? = nil
    @State private var labelsText = ""

    /// Parent options: all top-level tasks (potential epics).
    private var parentOptions: [WreckItTask] {
        store.tasks.filter { $0.parentId == nil }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("New Task")
                .font(.title2)
                .fontWeight(.semibold)

            Form {
                TextField("ID", text: $id)
                    .textFieldStyle(.roundedBorder)

                TextField("Description", text: $description, axis: .vertical)
                    .textFieldStyle(.roundedBorder)
                    .lineLimit(3...6)

                Picker("Parent (Epic)", selection: $selectedParent) {
                    Text("None (top-level)").tag(nil as String?)
                    ForEach(parentOptions) { task in
                        Text("\(task.id) — \(task.description)").tag(task.id as String?)
                    }
                }

                TextField("Labels (comma-separated)", text: $labelsText)
                    .textFieldStyle(.roundedBorder)
            }

            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Create") {
                    let labels = labelsText
                        .split(separator: ",")
                        .map { $0.trimmingCharacters(in: .whitespaces) }
                        .filter { !$0.isEmpty }
                    store.createTask(
                        id: id.isEmpty ? String(UUID().uuidString.prefix(8).lowercased()) : id,
                        description: description,
                        parentId: selectedParent,
                        labels: labels
                    )
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(description.isEmpty)
            }
        }
        .padding(20)
        .frame(minWidth: 400)
    }
}

#Preview {
    NewTaskSheet()
        .environmentObject(ProjectStore.preview)
}
