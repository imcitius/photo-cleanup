// Settings → the desktop app's data folder: where the database and the
// thumbnail cache live, how big they are, and moving them elsewhere.
//
// Only in the desktop window. Moving is the shell's job (it has to stop this
// very server), and it goes: pick a folder → preview, which touches nothing →
// an explicit button → copy, verify, restart. Cancel at any point before the
// button changes nothing. The old folder is never deleted; the screen says
// where it stays. Photographs are not part of this at all.
import { useEffect, useState } from "react";
import { Button, ErrorBox, Modal, Notice } from "./components";
import {
  changeDataDir,
  desktopInfo,
  pickFolder,
  previewDataDirChange,
  revealDataDir,
  type DesktopInfo,
  type MovePreview,
} from "./desktop";
import { bytes, number, ui } from "./i18n";

export function DataFolder({ disabled }: { disabled: boolean }) {
  const [info, setInfo] = useState<DesktopInfo | null>(null),
    [error, setError] = useState(""),
    [preview, setPreview] = useState<MovePreview | null>(null);
  const load = () =>
    desktopInfo().then(setInfo, (e) => setError((e as Error).message));
  useEffect(() => {
    load();
  }, []);
  const choose = async () => {
    setError("");
    try {
      const target = await pickFolder(info?.layout.dir, ui.desktop.chooseTitle);
      if (!target) return;
      setPreview(await previewDataDirChange(target));
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const offer = async (target: string) => {
    setError("");
    try {
      setPreview(await previewDataDirChange(target));
    } catch (e) {
      setError((e as Error).message);
    }
  };
  return (
    <section className="panel" aria-labelledby="data-folder-title">
      <h3 id="data-folder-title">{ui.desktop.title}</h3>
      {info && (
        <>
          <p className="muted">{ui.desktop.modes[info.source]}</p>
          <label className="field">
            {ui.desktop.folder}
            <input readOnly value={info.layout.dir} />
          </label>
          <label className="field">
            {ui.desktop.database}
            <input readOnly value={info.layout.db} />
          </label>
          <label className="field">
            {ui.desktop.thumbs}
            <input readOnly value={info.layout.thumbs} />
          </label>
          <p className="muted" data-testid="data-folder-size">
            {info.size
              ? ui.desktop.size
                  .replace("{0}", bytes(info.size.db_bytes))
                  .replace("{1}", bytes(info.size.thumbs_bytes))
                  .replace("{2}", number(info.size.thumbs_files))
              : ui.desktop.sizeUnknown}
            {info.available != null &&
              " " + ui.desktop.free.replace("{0}", bytes(info.available))}
          </p>
          <div className="inline">
            <Button
              type="button"
              icon="folder"
              onClick={() =>
                revealDataDir().catch((e) => setError((e as Error).message))
              }
            >
              {ui.desktop.reveal}
            </Button>
            {info.can_change && (
              <Button type="button" disabled={disabled} onClick={choose}>
                {ui.desktop.change}
              </Button>
            )}
          </div>
          {info.can_change && info.source !== "system" && (
            <Button
              type="button"
              disabled={disabled}
              onClick={() => offer(info.system_dir)}
            >
              {ui.desktop.useSystem}
            </Button>
          )}
          {!info.can_change && <p className="muted">{ui.desktop.fixed}</p>}
          {info.legacy_dir && (
            <Notice>
              {ui.desktop.legacy.replace("{0}", info.legacy_dir)}{" "}
              <Button
                type="button"
                disabled={disabled}
                onClick={() => offer(info.legacy_dir!)}
              >
                {ui.desktop.legacyAction}
              </Button>
            </Notice>
          )}
          <p className="muted">{ui.desktop.photosUntouched}</p>
        </>
      )}
      {error && <ErrorBox message={error} />}
      {preview && (
        <ChangeDialog
          preview={preview}
          onClose={() => {
            setPreview(null);
            load();
          }}
        />
      )}
    </section>
  );
}

function ChangeDialog({
  preview: p,
  onClose,
}: {
  preview: MovePreview;
  onClose: () => void;
}) {
  const [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  // An existing database may have WAL/SHM or empty reservations left by a
  // verified copy after a failed restart. These prevent copying over it,
  // not the explicit switch. Still require existing_database below: sidecars
  // alone (or a symlink in place of the database) are not a usable target.
  const onlyOccupied = p.blockers.every(
    (b) =>
      b.kind === "database_exists" ||
      b.kind === "thumbs_exist" ||
      b.kind === "sidecar_exists",
  );
  const run = async (action: "copy" | "use_existing") => {
    setBusy(true);
    setError("");
    try {
      await changeDataDir(p.to, action);
      // Success restarts the app; if this page is still here, it is about
      // to go.
    } catch (e) {
      setError((e as Error).message);
      setBusy(false);
    }
  };
  return (
    <Modal title={ui.desktop.previewTitle} onClose={busy ? () => {} : onClose}>
      <dl className="data-folder-preview">
        <dt>{ui.desktop.from}</dt>
        <dd>
          <code>{p.from}</code>
        </dd>
        <dt>{ui.desktop.to}</dt>
        <dd>
          <code>{p.to}</code>
        </dd>
        <dt>{ui.desktop.toCopy}</dt>
        <dd>
          {ui.desktop.size
            .replace("{0}", bytes(p.size.db_bytes))
            .replace("{1}", bytes(p.size.thumbs_bytes))
            .replace("{2}", number(p.size.thumbs_files))}
        </dd>
        <dt>{ui.desktop.space}</dt>
        <dd>
          {ui.desktop.spaceValue
            .replace("{0}", bytes(p.needed))
            .replace("{1}", p.available == null ? "?" : bytes(p.available))}
        </dd>
      </dl>
      {p.reasons.length > 0 && (
        <Notice tone="warning">
          <ul className="data-folder-reasons">
            {p.reasons.map((r) => (
              <li key={r}>{r}</li>
            ))}
          </ul>
        </Notice>
      )}
      <p className="muted">{ui.desktop.howItGoes}</p>
      {busy && (
        <p role="status" className="loading">
          <span className="spinner" />
          {ui.desktop.working}
        </p>
      )}
      {error && <ErrorBox message={ui.desktop.failed.replace("{0}", error)} />}
      <div className="modal-actions">
        <Button type="button" disabled={busy} onClick={onClose}>
          {ui.cancel}
        </Button>
        {p.existing_database && onlyOccupied && (
          <Button
            type="button"
            disabled={busy}
            onClick={() => run("use_existing")}
          >
            {ui.desktop.useExisting}
          </Button>
        )}
        {!p.existing_database && (
          <Button
            type="button"
            kind="primary"
            disabled={busy || p.blockers.length > 0}
            onClick={() => run("copy")}
          >
            {ui.desktop.copy}
          </Button>
        )}
      </div>
    </Modal>
  );
}
