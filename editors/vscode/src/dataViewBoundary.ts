const DATA_GENERATION_PROFILE_VERSION = "data.generation.v1";

export type StoreSnapshot = DataGeneration;

export interface DataGeneration {
  readonly profile_version: typeof DATA_GENERATION_PROFILE_VERSION;
  readonly store_uid: string | null;
  readonly catalog_digest: string | null;
  readonly commit: DataGenerationCommit | null;
  readonly open_transaction: DataGenerationTransaction | null;
  readonly checked_source_digest: string;
}

export interface DataGenerationCommit {
  readonly commit_id: number;
  readonly catalog_epoch: number;
  readonly source_digest: string;
  readonly layout_epoch: number;
  readonly engine_profile_digest: string;
}

export interface DataGenerationTransaction {
  readonly depth: number;
}

export interface SourceAnalysisGeneration {
  readonly profileVersion: "analysis.generation.v1";
  readonly sourceIdentity: string;
  readonly configDigest: string;
  readonly checkedSourceDigest: string;
  readonly readOnlyContextDigest: string;
  readonly acceptedCatalog: SourceAnalysisCatalog | null;
  readonly proposalCatalog: SourceAnalysisCatalog | null;
}

export interface SourceAnalysisCatalog {
  readonly epoch: number;
  readonly digest: string | null;
}

export interface DataViewBoundary {
  readonly sourceAnalysisGeneration: SourceAnalysisGeneration;
  readonly storeSnapshot: StoreSnapshot;
  readonly compatibility: { readonly verdict: "admitted" };
  readonly watchTargets: DataViewWatchTarget[];
}

export interface DataViewWatchTarget {
  readonly kind: "store_file" | "catalog_lock";
  readonly path: string;
}

export function dataViewBoundaryFromWire(value: unknown): DataViewBoundary | undefined {
  if (!isRecord(value)) {
    return undefined;
  }
  const sourceAnalysisGeneration = sourceAnalysisGenerationFromWire(
    value.sourceAnalysisGeneration,
  );
  const storeSnapshot = storeSnapshotFromWire(value.storeSnapshot);
  const compatibility = compatibilityFromWire(value.compatibility);
  const watchTargets = watchTargetsFromWire(value.watchTargets);
  if (
    sourceAnalysisGeneration === undefined ||
    storeSnapshot === undefined ||
    compatibility === undefined ||
    watchTargets === undefined
  ) {
    return undefined;
  }
  return {
    sourceAnalysisGeneration,
    storeSnapshot,
    compatibility,
    watchTargets,
  };
}

export function sameBoundary(left: DataViewBoundary, right: DataViewBoundary): boolean {
  return (
    sameSourceAnalysisGeneration(
      left.sourceAnalysisGeneration,
      right.sourceAnalysisGeneration,
    ) &&
    sameSnapshot(left.storeSnapshot, right.storeSnapshot) &&
    left.compatibility.verdict === right.compatibility.verdict &&
    sameWatchTargets(left.watchTargets, right.watchTargets)
  );
}

export function sameStoreSnapshot(left: StoreSnapshot, right: StoreSnapshot): boolean {
  return sameSnapshot(left, right);
}

function sourceAnalysisGenerationFromWire(
  value: unknown,
): SourceAnalysisGeneration | undefined {
  if (!isRecord(value)) {
    return undefined;
  }
  if (
    value.profileVersion !== "analysis.generation.v1" ||
    typeof value.sourceIdentity !== "string" ||
    typeof value.configDigest !== "string" ||
    typeof value.checkedSourceDigest !== "string" ||
    typeof value.readOnlyContextDigest !== "string"
  ) {
    return undefined;
  }
  const acceptedCatalog = sourceAnalysisCatalogFromWire(value.acceptedCatalog);
  const proposalCatalog = sourceAnalysisCatalogFromWire(value.proposalCatalog);
  if (acceptedCatalog === undefined || proposalCatalog === undefined) {
    return undefined;
  }
  return {
    profileVersion: "analysis.generation.v1",
    sourceIdentity: value.sourceIdentity,
    configDigest: value.configDigest,
    checkedSourceDigest: value.checkedSourceDigest,
    readOnlyContextDigest: value.readOnlyContextDigest,
    acceptedCatalog,
    proposalCatalog,
  };
}

function sourceAnalysisCatalogFromWire(value: unknown): SourceAnalysisCatalog | null | undefined {
  if (value === null) {
    return null;
  }
  if (!isRecord(value) || !isNonNegativeSafeInteger(value.epoch) || !isStringOrNull(value.digest)) {
    return undefined;
  }
  return { epoch: value.epoch, digest: value.digest };
}

function compatibilityFromWire(
  value: unknown,
): { readonly verdict: "admitted" } | undefined {
  if (!isRecord(value) || value.verdict !== "admitted") {
    return undefined;
  }
  return { verdict: "admitted" };
}

function watchTargetsFromWire(value: unknown): DataViewWatchTarget[] | undefined {
  if (!Array.isArray(value)) {
    return undefined;
  }
  const targets: DataViewWatchTarget[] = [];
  for (const target of value) {
    if (
      !isRecord(target) ||
      (target.kind !== "store_file" && target.kind !== "catalog_lock") ||
      typeof target.path !== "string"
    ) {
      return undefined;
    }
    targets.push({ kind: target.kind, path: target.path });
  }
  return targets;
}

export function storeSnapshotFromWire(value: unknown): StoreSnapshot | undefined {
  if (!isRecord(value)) {
    return undefined;
  }
  if (value.profile_version !== DATA_GENERATION_PROFILE_VERSION) {
    return undefined;
  }
  if (
    !isStringOrNull(value.store_uid) ||
    !isStringOrNull(value.catalog_digest) ||
    typeof value.checked_source_digest !== "string"
  ) {
    return undefined;
  }
  const commit = generationCommitFromWire(value.commit);
  if (commit === undefined) {
    return undefined;
  }
  const transaction = generationTransactionFromWire(value.open_transaction);
  if (transaction === undefined) {
    return undefined;
  }
  return {
    profile_version: DATA_GENERATION_PROFILE_VERSION,
    store_uid: value.store_uid,
    catalog_digest: value.catalog_digest,
    commit,
    open_transaction: transaction,
    checked_source_digest: value.checked_source_digest,
  };
}

function generationCommitFromWire(value: unknown): DataGenerationCommit | null | undefined {
  if (value === null) {
    return null;
  }
  if (!isRecord(value)) {
    return undefined;
  }
  if (
    !isNonNegativeSafeInteger(value.commit_id) ||
    !isNonNegativeSafeInteger(value.catalog_epoch) ||
    typeof value.source_digest !== "string" ||
    !isNonNegativeSafeInteger(value.layout_epoch) ||
    typeof value.engine_profile_digest !== "string"
  ) {
    return undefined;
  }
  return {
    commit_id: value.commit_id,
    catalog_epoch: value.catalog_epoch,
    source_digest: value.source_digest,
    layout_epoch: value.layout_epoch,
    engine_profile_digest: value.engine_profile_digest,
  };
}

function generationTransactionFromWire(
  value: unknown,
): DataGenerationTransaction | null | undefined {
  if (value === null) {
    return null;
  }
  if (!isRecord(value) || !isNonNegativeSafeInteger(value.depth)) {
    return undefined;
  }
  return { depth: value.depth };
}

function sameWatchTargets(
  left: DataViewWatchTarget[],
  right: DataViewWatchTarget[],
): boolean {
  return (
    left.length === right.length &&
    left.every((target, index) => {
      const other = right[index];
      return (
        other !== undefined &&
        target.kind === other.kind &&
        target.path === other.path
      );
    })
  );
}

function sameSourceAnalysisGeneration(
  left: SourceAnalysisGeneration,
  right: SourceAnalysisGeneration,
): boolean {
  return (
    left.profileVersion === right.profileVersion &&
    left.sourceIdentity === right.sourceIdentity &&
    left.configDigest === right.configDigest &&
    left.checkedSourceDigest === right.checkedSourceDigest &&
    left.readOnlyContextDigest === right.readOnlyContextDigest &&
    sameSourceAnalysisCatalog(left.acceptedCatalog, right.acceptedCatalog) &&
    sameSourceAnalysisCatalog(left.proposalCatalog, right.proposalCatalog)
  );
}

function sameSourceAnalysisCatalog(
  left: SourceAnalysisCatalog | null,
  right: SourceAnalysisCatalog | null,
): boolean {
  if (left === null || right === null) {
    return left === right;
  }
  return left.epoch === right.epoch && left.digest === right.digest;
}

function sameSnapshot(left: StoreSnapshot, right: StoreSnapshot): boolean {
  if (left === right) {
    return true;
  }
  return (
    left.profile_version === right.profile_version &&
    left.store_uid === right.store_uid &&
    left.catalog_digest === right.catalog_digest &&
    sameCommit(left.commit, right.commit) &&
    sameTransaction(left.open_transaction, right.open_transaction) &&
    left.checked_source_digest === right.checked_source_digest
  );
}

function sameCommit(
  left: DataGenerationCommit | null,
  right: DataGenerationCommit | null,
): boolean {
  if (left === right) {
    return true;
  }
  if (left === null || right === null) {
    return false;
  }
  return (
    left.commit_id === right.commit_id &&
    left.catalog_epoch === right.catalog_epoch &&
    left.source_digest === right.source_digest &&
    left.layout_epoch === right.layout_epoch &&
    left.engine_profile_digest === right.engine_profile_digest
  );
}

function sameTransaction(
  left: DataGenerationTransaction | null,
  right: DataGenerationTransaction | null,
): boolean {
  if (left === right) {
    return true;
  }
  if (left === null || right === null) {
    return false;
  }
  return left.depth === right.depth;
}

function isNonNegativeSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isStringOrNull(value: unknown): value is string | null {
  return value === null || typeof value === "string";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
