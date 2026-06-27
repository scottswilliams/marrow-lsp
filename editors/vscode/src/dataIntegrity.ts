import * as vscode from "vscode";
import { LanguageClient } from "vscode-languageclient/node";
import {
  dataViewBoundaryFromWire,
  sameStoreSnapshot,
  storeSnapshotFromWire,
  type DataViewBoundary,
  type SourceAnalysisCatalog,
} from "./dataViewBoundary";

// These types mirror the marrow-lsp custom request contract EXACTLY. They are
// the wire shapes the server speaks; do not "improve" them or they will drift
// from the server. The TypeScript side never interprets a path or a finding —
// it only ferries what the core reports, which owns all store and schema
// semantics.

interface IntegritySourceSpan {
  // Root-first store path the finding concerns, rendered by the core.
  readonly path: string;
}

type IntegritySavedKey =
  | { readonly type: "int"; readonly value: number }
  | { readonly type: "bool"; readonly value: boolean }
  | { readonly type: "string"; readonly value: string }
  | { readonly type: "date"; readonly days_since_epoch: number }
  | { readonly type: "duration"; readonly nanos: string }
  | { readonly type: "instant"; readonly nanos_since_epoch: string }
  | { readonly type: "bytes"; readonly value_b64: string };

type IntegrityPathSegment =
  | { readonly member_catalog_id: string }
  | { readonly key: IntegritySavedKey };

interface Finding {
  // Canonical Marrow tooling problem code, owned by the core.
  readonly code: string;
  readonly kind: string;
  // Human-readable explanation, owned by the core.
  readonly message: string;
  readonly source_span: IntegritySourceSpan;
  // Optional remediation help from the core.
  readonly help?: string | null;
  readonly store_catalog_id?: string;
  readonly record_identity?: readonly IntegritySavedKey[];
  readonly parent_path?: readonly IntegrityPathSegment[];
  readonly missing_member_catalog_id?: string;
  readonly containing_identity?: readonly IntegritySavedKey[];
  readonly field_catalog_id?: string;
  readonly referenced_root?: string;
  readonly referenced_identity?: readonly IntegritySavedKey[];
}

interface DataIntegrityResult {
  available: boolean;
  findings: Finding[];
  scanned: number;
  truncated: boolean;
  store_snapshot: unknown;
  data_view_boundary?: unknown;
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

    const boundary = dataViewBoundaryFromWire(result.data_view_boundary);
    if (boundary === undefined) {
      this.showUnavailable();
      return;
    }
    const storeSnapshot = storeSnapshotFromWire(result.store_snapshot);
    if (
      storeSnapshot === undefined ||
      !sameStoreSnapshot(storeSnapshot, boundary.storeSnapshot)
    ) {
      this.showUnavailable();
      return;
    }

    this.report(result, boundary);
  }

  private showUnavailable(): void {
    this.clearReport();
    void vscode.window.showInformationMessage(
      "Advisory data-integrity check needs opt-in live data and a readable native dev-store " +
        "(enable marrow.liveData / ensure marrow.json points at a native store)",
    );
  }

  private clearReport(): void {
    if (this.channel === undefined) {
      return;
    }
    this.channel.clear();
    this.channel.appendLine("Data view unavailable");
  }

  private report(result: DataIntegrityResult, boundary: DataViewBoundary): void {
    const channel = this.getChannel();
    channel.clear();
    channel.appendLine(formatBoundary(boundary));

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

// One report line per finding. Display only — the core owns the semantics behind
// each field.
function formatFinding(finding: Finding): string {
  const help =
    finding.help === undefined || finding.help === null
      ? ""
      : ` Help: ${finding.help}`;
  return `[${finding.code}] ${finding.source_span.path}: ${finding.message}${help}`;
}

function formatBoundary(boundary: DataViewBoundary): string {
  const analysis = boundary.sourceAnalysisGeneration;
  return [
    "Data view:",
    `${boundary.compatibility.verdict} source ${analysis.sourceIdentity};`,
    `checked ${analysis.checkedSourceDigest};`,
    `accepted catalog ${formatCatalog(analysis.acceptedCatalog)};`,
    `store ${formatStoreCommit(boundary)};`,
    `watch targets ${boundary.watchTargets.length}`,
  ].join(" ");
}

function formatCatalog(catalog: SourceAnalysisCatalog | null): string {
  if (catalog === null) {
    return "none";
  }
  return `${catalog.epoch} ${catalog.digest ?? "no digest"}`;
}

function formatStoreCommit(boundary: DataViewBoundary): string {
  const commit = boundary.storeSnapshot.commit;
  if (commit === null) {
    return "no commit";
  }
  return `commit ${commit.commit_id}`;
}
