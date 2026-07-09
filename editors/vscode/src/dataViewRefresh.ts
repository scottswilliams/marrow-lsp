// Coalesce and gate the saved-data view's refresh so a burst of diagnostics publishes or CLI
// commits does not storm the language server's read-only session opens. Kept free of vscode so the
// debounce and visibility contract can be exercised directly; the extension wires the tree view's
// visibility and the watch-target/tree refreshes into it.

export interface DataViewRefreshTargets {
  // Re-resolve the saved-data watch targets and re-register file watchers. Runs regardless of
  // visibility so a store change is still observed while the inspector is hidden.
  refreshWatchTargets(): Promise<void>;
  // Re-query the saved-data tree from the server — the costly round-trip, so it is gated on the
  // inspector being on screen.
  refreshTree(): void;
  // Whether the saved-data inspector is currently visible.
  isInspectorVisible(): boolean;
}

// Debounces the saved-data view refresh: rapid triggers within a window collapse into a single
// refresh, and the tree round-trip is skipped while the inspector is hidden.
export class DataViewRefresher {
  private timer: ReturnType<typeof setTimeout> | undefined;
  private disposed = false;

  constructor(
    private readonly targets: DataViewRefreshTargets,
    private readonly debounceMs: number,
  ) {}

  // Coalesce a data-view refresh: calls within the debounce window fold into one, so a per-publish
  // or per-commit storm becomes at most one refresh per window.
  schedule(): void {
    if (this.disposed || this.timer !== undefined) {
      return;
    }
    this.timer = setTimeout(() => {
      this.timer = undefined;
      void this.run();
    }, this.debounceMs);
  }

  dispose(): void {
    this.disposed = true;
    if (this.timer !== undefined) {
      clearTimeout(this.timer);
      this.timer = undefined;
    }
  }

  private async run(): Promise<void> {
    if (this.disposed) {
      return;
    }
    await this.targets.refreshWatchTargets();
    if (this.targets.isInspectorVisible()) {
      this.targets.refreshTree();
    }
  }
}

// Whether two resolved watch-target path lists are the same in order, so an unchanged target set
// skips tearing down and recreating identical file watchers.
export function sameWatchTargets(previous: readonly string[], next: readonly string[]): boolean {
  return previous.length === next.length && previous.every((path, index) => path === next[index]);
}
