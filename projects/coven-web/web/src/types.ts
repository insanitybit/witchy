// Wire types mirroring coven's frozen HTTP contract (PLAN.md §4).
export type State = "released" | "staged" | "yanked";

export interface Record {
  name: string;
  version: string;
  state: State;
  hash: string;
  runtime_footprint: string[];
  build_footprint: string[];
  determinism: string;
  uploaded_by: string;
  promoted_by: string | null;
  second_factor: string | null;
  provenance: string | null;
  released_at: number;
  sig: string;
}

export interface IndexResp { names: string[]; }
export interface VersionsResp { records: Record[]; }

// A source bundle: [relative path, UTF-8 text] pairs, content-hash-verified by coven.
export type SourceFile = [string, string];
export interface SourceResp { files: SourceFile[]; }

// TUF roles (PLAN.md §4). The snapshot is a version-numbered manifest of every
// target's digest (rollback detection); the timestamp is a short-lived pointer to
// it (freeze detection). Both are Ed25519-signed by the registry root key.
export interface Snapshot {
  signed: { version: number; created: number; targets: { [nameAtVersion: string]: string } };
  sig: string;
}
export interface Timestamp {
  signed: { snapshot_version: number; snapshot_hash: string; expires: number };
  sig: string;
}
