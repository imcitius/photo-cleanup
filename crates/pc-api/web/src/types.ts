export interface Member {
  file_id: number;
  name: string;
  dir: string;
  role: string;
  role_label: string;
  size: number;
  width: number;
  height: number;
  quality: number;
  breakdown: string;
  evidence: { detail?: string } | null;
  thumb: string | null;
  is_keeper: boolean;
  /// Whether this file holds the same pixels as the one being kept.
  same_as_kept: boolean;
  sidecars?: string[];
  catalogs?: string[];
  rating?: number | null;
}
export interface Family {
  id: number;
  taken_at: number | null;
  camera: string | null;
  total_size: number;
  removable_bytes: number;
  members: Member[];
}
export interface Status {
  version: string;
  without_thumb: number;
  files: number;
  images: number;
  skipped: number;
  families: number;
  families_multi: number;
  series: number;
  categorised: number;
  roles: {
    role: string;
    label: string;
    count: number;
    bytes: number;
    removable: boolean;
  }[];
  derived_removable_bytes: number;
  derived_blocked: number;
  quarantined_bytes: number;
  mislabelled: number;
}
/** One row of the index, as the live strip shows it while a pass runs. */
export interface RecentFile {
  id: number;
  path: string;
  name: string;
  size: number;
  width: number | null;
  height: number | null;
  thumb_key: string | null;
  skipped_reason: string | null;
}
/** Everything the index recorded about one file, behind the score. */
export interface FileDetails {
  id: number;
  path: string;
  name: string;
  disk: string;
  size: number;
  mtime: number;
  container: string | null;
  extension_lied: number;
  width: number | null;
  height: number | null;
  orientation: number | null;
  pixel_source: string | null;
  state: string;
  skipped_reason: string | null;
  sharpness: number | null;
  clip_low: number | null;
  clip_high: number | null;
  entropy: number | null;
  contrast: number | null;
  saturation: number | null;
  chroma: number | null;
  tonal_range: number | null;
  white_fraction: number | null;
  bimodality: number | null;
  text_rows: number | null;
  text_banding: number | null;
  meta: {
    taken_at: number | null;
    date_source: string | null;
    camera_make: string | null;
    camera_model: string | null;
    body_serial: string | null;
    lens: string | null;
    iso: number | null;
    f_number: number | null;
    focal_length: number | null;
    exposure: string | null;
    gps_lat: number | null;
    gps_lon: number | null;
    software: string | null;
    xmp_document_id: string | null;
    xmp_original_id: string | null;
    xmp_derived_from: string | null;
    dng_original_raw: string | null;
  } | null;
  categories: {
    category: string;
    confidence: number;
    evidence: string | null;
    manual: number;
  }[];
}
export interface Progress {
  phase: string;
  done: number;
  total: number;
  bytes_done: number;
  bytes_total: number;
  current: string;
  per_disk: { disk: string; done: number; total: number }[];
  eta_secs: number | null;
  note: string;
  /** Position in a multi-stage job, 1-based; 0 when there is only one. */
  step: number;
  steps: number;
  refusals: [string, string][];
}
export interface Job {
  id: number;
  kind: string;
  params: Record<string, unknown>;
  state: string;
  progress: Partial<Progress>;
  started_at: number;
  finished_at: number | null;
  error: string | null;
  run_id: number | null;
}
export interface Settings {
  db_path: string;
  thumbs_path: string;
  quarantine: string | null;
  roots: string[];
  /** Folders the server can see, offered when none are chosen yet. */
  suggested_roots: string[];
  phash_max: number;
  ssim_min: number;
  min_size: number;
  series_gap_secs: number;
  event_gap_secs: number;
  /** Decoding threads. 0 leaves one core free for the rest of the machine. */
  workers: number;
  /** Cores this server can see, for the upper bound on `workers`. */
  cores: number;
  language: "ru" | "en";
  theme: "light" | "dark" | "system";
  density: "comfortable" | "compact";
  network: boolean;
}
export interface PlanItem {
  file_id?: number;
  journal_id?: number;
  path: string;
  dst: string;
  size: number;
  file_count: number;
  role?: string;
  /** Put on the list by the user, not by its role in a family. */
  manual?: boolean;
  reason?: string;
  keeper_id?: number;
  keeper_path?: string;
  thumb?: string | null;
  keeper_thumb?: string | null;
  source?: string;
  uncertain?: boolean;
  taken_at?: number;
  event?: string;
  year?: number;
  renamed_from?: string | null;
  event_cover?: string | null;
  companions?: { path: string; dst: string; size: number }[];
}
export interface Preview {
  kind: string;
  params: Record<string, unknown>;
  token: string;
  items: PlanItem[];
  refusals: { path: string; why: string }[];
  total_files: number;
  total_bytes: number;
}
export interface Bundle {
  id: number;
  path: string;
  kind: string;
  kind_label: string;
  file_count: number;
  size: number;
  removable: boolean;
  regenerable: boolean;
  blocked: string | null;
  hint: string | null;
  state: string;
}
export interface Journal {
  id: number;
  run_id: number;
  op: string;
  src: string;
  dst: string | null;
  size: number;
  file_count: number;
  status: string;
  applied_at: number;
  note: string | null;
}
export interface Run {
  id: number;
  started_at: number;
  finished_at: number | null;
  roots: string[];
  operations: number;
  bytes: number;
  undoable: number | null;
}
export interface Catalog {
  id: number;
  name: string;
  path: string;
  disk: string;
  size: number;
  is_locked: boolean;
  is_backup: number;
  image_count: number | null;
  read_error: string | null;
}
/** One entry sitting in quarantine, with enough to look at it. */
export interface QuarantineItem {
  journal_id: number;
  file_id: number | null;
  src: string;
  dst: string | null;
  name: string;
  size: number;
  file_count: number;
  applied_at: number;
  kind: string;
  thumb: string | null;
}
export interface Series {
  id: number;
  kind: string;
  label: string;
  started_at: number | null;
  camera: string | null;
  protected: boolean;
  members: {
    file_id: number;
    name: string;
    dir: string;
    rank: number;
    score: number;
    breakdown: string;
    sharpness: number | null;
    thumb: string | null;
    taken_at: number | null;
    is_best: boolean;
    is_rejected: boolean;
    family_id: number | null;
    family_size: number;
    is_family_keeper: boolean;
  }[];
}
export interface Category {
  key: string;
  label: string;
  count: number;
  bytes: number;
  files: {
    file_id: number;
    name: string;
    dir: string;
    size: number;
    width: number;
    height: number;
    confidence: number;
    manual: boolean;
    evidence: string;
    thumb: string | null;
  }[];
}
export type Page =
  | "overview"
  | "setup"
  | "families"
  | "series"
  | "categories"
  | "plan"
  | "organize"
  | "derived"
  | "quarantine"
  | "journal"
  | "settings";
