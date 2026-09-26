// The desktop window's own commands, when this page runs inside it.
//
// The same interface is served to a browser, to a NAS and to the desktop
// window. Only the window has Tauri's IPC, and only for this server's exact
// origin (pc-desktop grants it at run time), so its presence is the test:
// in a browser everything here is absent and the server-side folder browser
// is used as before. No @tauri-apps/api dependency — five calls do not need
// a package, and nothing here is loaded from the network.
//
// Business operations never go through here: they stay on the HTTP API with
// its preview token and writer lock. These are what a web page cannot do —
// a native folder dialog, showing a folder in Finder/Explorer, and moving
// the app's data, which needs the server stopped.

type Internals = {
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
};

const ipc = (window as unknown as { __TAURI_INTERNALS__?: Internals })
  .__TAURI_INTERNALS__;

export const isDesktop = !!ipc?.invoke;

function invoke<T>(command: string, args?: Record<string, unknown>) {
  if (!ipc) return Promise.reject(new Error("not in the desktop window"));
  // The shell answers errors as text in the interface's language.
  return ipc.invoke<T>(command, args).catch((e: unknown) => {
    throw new Error(String(e));
  });
}

export type Source = "override" | "portable" | "system" | "custom";

export type DataSize = {
  db_bytes: number;
  thumbs_bytes: number;
  thumbs_files: number;
};

export type DesktopInfo = {
  source: Source;
  can_change: boolean;
  layout: { dir: string; db: string; thumbs: string };
  size: DataSize | null;
  available: number | null;
  system_dir: string;
  legacy_dir: string | null;
};

export type MovePreview = {
  from: string;
  to: string;
  size: DataSize;
  needed: number;
  available: number | null;
  blockers: { kind: string }[];
  reasons: string[];
  existing_database: boolean;
};

/** A native folder dialog; `null` when cancelled. */
export const pickFolder = (initial?: string, title?: string) =>
  invoke<string | null>("pick_folder", {
    initial: initial || null,
    title: title || null,
  });

export const desktopInfo = () => invoke<DesktopInfo>("desktop_info");

export const revealDataDir = () => invoke<void>("reveal_data_dir");

export const previewDataDirChange = (target: string) =>
  invoke<MovePreview>("preview_data_dir_change", { target });

/**
 * On success the app restarts and this page goes with it, so the promise
 * matters only when it rejects: the old folder is back in use, and the
 * error says why.
 */
export const changeDataDir = (
  target: string,
  action: "copy" | "use_existing",
) => invoke<void>("change_data_dir", { target, action });
