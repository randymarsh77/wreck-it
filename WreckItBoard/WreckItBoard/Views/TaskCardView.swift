import SwiftUI

/// A card representing a single task on the board.
struct TaskCardView: View {
    @EnvironmentObject var store: ProjectStore
    let task: WreckItTask
    @State private var showDetail = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            // Header: id + parent badge
            HStack {
                Text(task.id)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                if let pid = task.parentId {
                    Text(pid)
                        .font(.caption2)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Color.purple.opacity(0.15))
                        .clipShape(Capsule())
                }
            }

            // Description
            Text(task.description)
                .font(.subheadline)
                .lineLimit(3)

            // Labels
            if let labels = task.labels, !labels.isEmpty {
                FlowLayout(spacing: 4) {
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

            // Quick-action: move to next status
            HStack(spacing: 4) {
                if task.status != .completed {
                    Button {
                        let next = nextStatus(from: task.status)
                        store.moveTask(id: task.id, to: next)
                    } label: {
                        Label(nextStatus(from: task.status).displayName,
                              systemImage: "arrow.right.circle")
                            .font(.caption)
                    }
                    .buttonStyle(.borderless)
                }
                Spacer()
                Button {
                    showDetail = true
                } label: {
                    Image(systemName: "info.circle")
                        .font(.caption)
                }
                .buttonStyle(.borderless)
            }
        }
        .padding(10)
        .background(Color(nsColor: .textBackgroundColor))
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .shadow(color: .black.opacity(0.05), radius: 2, y: 1)
        .sheet(isPresented: $showDetail) {
            TaskDetailView(task: task)
        }
    }

    private func nextStatus(from current: TaskStatus) -> TaskStatus {
        switch current {
        case .pending:    return .inprogress
        case .inprogress: return .completed
        case .completed:  return .completed
        case .failed:     return .pending
        }
    }
}

/// Simple flow layout for labels (wraps to next line).
struct FlowLayout: Layout {
    var spacing: CGFloat = 4

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let result = layout(in: proposal.width ?? .infinity, subviews: subviews)
        return result.size
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let result = layout(in: bounds.width, subviews: subviews)
        for (index, offset) in result.offsets.enumerated() {
            subviews[index].place(at: CGPoint(x: bounds.minX + offset.x,
                                               y: bounds.minY + offset.y),
                                   proposal: .unspecified)
        }
    }

    private func layout(in maxWidth: CGFloat, subviews: Subviews) -> (offsets: [CGPoint], size: CGSize) {
        var offsets: [CGPoint] = []
        var x: CGFloat = 0
        var y: CGFloat = 0
        var rowHeight: CGFloat = 0
        var maxX: CGFloat = 0

        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x + size.width > maxWidth, x > 0 {
                x = 0
                y += rowHeight + spacing
                rowHeight = 0
            }
            offsets.append(CGPoint(x: x, y: y))
            rowHeight = max(rowHeight, size.height)
            x += size.width + spacing
            maxX = max(maxX, x)
        }

        return (offsets, CGSize(width: maxX, height: y + rowHeight))
    }
}

#Preview {
    TaskCardView(task: WreckItTask.samples[1])
        .environmentObject(ProjectStore.preview)
        .frame(width: 260)
        .padding()
}
