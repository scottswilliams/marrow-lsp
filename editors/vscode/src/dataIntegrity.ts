import * as vscode from "vscode";
import { LanguageClient } from "vscode-languageclient/node";

// These types mirror the marrow-lsp custom request contract EXACTLY. They are
// the wire shapes the server speaks; do not "improve" them or they will drift
// from the server. The TypeScript side never interprets a path or a finding —
// it only ferries what the core reports, which owns all store and schema
// semantics.

interface Finding {
  // Root-first store path the finding concerns, rendered by the core.
  readonly path: string;
  // Short category of the issue (e.g. "type-mismatch"), owned by the core.
  readonly kind: string;
  // Human-readable explanation, owned by the core.
  readonly message: string;
  // Optional expected/actual type the core attaches when relevant.
  readonly type?: string;
}

interface DataIntegrityResult {
  available: boolean;
  findings: Finding[];
  scanned: number;
  truncated: boolean;
}

const REQUEST_DATA_INTEGRITY = "marrow/dataIntegrity";

const CHANNEL_NAME = "Marrow Data Integrity";

// A separate advisory view from the diagnostics/Problems panel. The integrity
// check is an opt-in report the user invokes; its findings are informational and
// must not pollute editor diagnostics. The output channel is created once and
// reused across invocations, and disposed with the extension.
export class MarrowDataIntegrity {
  private channel: vscode.OutputChannel | undefined;

  constructor(private readonly client: LanguageClient) {}

  // Run the integrity check and surface its result. Asking the core is the only
  // thing the TypeScript side does; it owns the scan, the schema, and every
  // finding. We just present what comes back.
  async run(): Promise<void> {
    let result: DataIntegrityResult;
    try {
      result = await this.client.sendRequest<DataIntegrityResult>(
        REQUEST_DATA_INTEGRITY,
      );
    } catch {
      // A read-only store may be transiently busy or unreadable; treat that as
      // the same soft "unavailable" state the rest of the extension uses rather
      // than surfacing an error to the user.
      this.showUnavailable();
      return;
    }

    if (!result.available) {
      this.showUnavailable();
      return;
    }

    this.report(result);
  }

  private showUnavailable(): void {
    void vscode.window.showInformationMessage(
      "Advisory data-integrity check needs opt-in live data and a readable native dev-store " +
        "(enable marrow.liveData / ensure marrow.json points at a native store)",
    );
  }

  private report(result: DataIntegrityResult): void {
    const channel = this.getChannel();
    channel.clear();

    const truncatedNote = result.truncated
      ? " (truncated; scan limit reached)"
      : "";
    channel.appendLine(
      `Scanned ${result.scanned} location(s)${truncatedNote}`,
    );

    for (const finding of result.findings) {
      channel.appendLine(formatFinding(finding));
    }

    channel.show(true);

    const count = result.findings.length;
    if (count === 0) {
      void vscode.window.showInformationMessage(
        "Advisory scan found no issues in the opt-in native dev-store read",
      );
    } else {
      void vscode.window.showInformationMessage(
        `${count} advisory issue(s) found`,
      );
    }
  }

  // The output channel is created lazily on first report and reused thereafter.
  private getChannel(): vscode.OutputChannel {
    if (this.channel === undefined) {
      this.channel = vscode.window.createOutputChannel(CHANNEL_NAME);
    }
    return this.channel;
  }

  dispose(): void {
    this.channel?.dispose();
    this.channel = undefined;
  }
}

// One report line per finding: kind, path, then message. The optional type is
// appended in parentheses when the core attaches it. Display only — the core
// owns the semantics behind each field.
function formatFinding(finding: Finding): string {
  const typeSuffix = finding.type !== undefined ? ` (${finding.type})` : "";
  return `[${finding.kind}] ${finding.path}: ${finding.message}${typeSuffix}`;
}
