// The archive as its owner already knows it: folders, opened one at a time.
//
// Every other screen here is a list of decisions the tool has prepared, sorted
// by something the tool measured. This one is the shape of the archive itself,
// and it exists for one sentence that nobody can say on the other screens:
// "the originals are in here". Said once about a folder, it settles every
// group that folder has a hand in — and it keeps settling them, because the
// mark is stored and applied again after every rebuild.
import { Fragment, useEffect, useState } from "react";
import { post, useResource } from "./api";
import {
  Button,
  Empty,
  Icon,
  Loading,
  Notice,
  Resource,
  Thumb,
  VirtualList,
} from "./components";
import { ImageViewer } from "./curation";
import { bytes, number, t, ui } from "./i18n";
import type { TreeView } from "./types";
import { Review } from "./workflow";
import type { Start } from "./workflow";

/** Where the last visit left off, so a reload does not start at the top. */
const LAST = "pc-tree-path";

/** The path split into the pieces a breadcrumb can be built from. */
function crumbs(path: string) {
  const out: { name: string; path: string }[] = [];
  const separator = /[/\\]/;
  let at = 0;
  while (at < path.length) {
    const rest = path.slice(at);
    const lead = rest.length - rest.replace(/^[/\\]+/, "").length;
    const body = rest.slice(lead);
    if (!body) break;
    const next = body.search(separator);
    const end = next === -1 ? path.length : at + lead + next;
    out.push({ name: path.slice(at + lead, end), path: path.slice(0, end) });
    at = end;
  }
  return out;
}

export function Tree({
  revision,
  disabled,
  start,
  onChange,
}: {
  revision: number;
  disabled: boolean;
  start: Start;
  onChange: () => void;
}) {
  const [path, setPath] = useState(() => localStorage.getItem(LAST) || ""),
    [nonce, setNonce] = useState(0),
    [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    // What the last mark did, kept until the next one: the numbers are the
    // whole answer to "did that do anything?", and they are gone from the
    // screen the moment the folder list redraws otherwise.
    [note, setNote] = useState<{ path: string; text: string } | null>(null),
    [viewing, setViewing] = useState<number | null>(null);
  const r = useResource<TreeView>(
    `/tree?path=${encodeURIComponent(path)}`,
    revision + nonce,
  );
  useEffect(() => {
    try {
      localStorage.setItem(LAST, path);
    } catch {
      // A private window forgets where we were. Nothing else breaks.
    }
  }, [path]);
  // A folder that has gone from the index — the archive was re-scanned
  // without it, or the path was typed by hand — would otherwise show an
  // empty page with no way out but the breadcrumb.
  const view = r.data;
  const go = (to: string) => {
    setPath(to);
    setError("");
    window.scrollTo(0, 0);
  };

  const mark = async (dir: string, marked: boolean) => {
    setBusy(true);
    setError("");
    try {
      const report = await post<{
        groups: number;
        moved: number;
        untouched: number;
      }>("/originals", { path: dir, marked });
      setNote({
        path: dir,
        text: marked
          ? [
              t("otmecheno_grupp", number(report.groups), number(report.moved)),
              report.untouched
                ? t("drugoy_kadr_a_ne_kopiya", number(report.untouched))
                : "",
            ]
              .filter(Boolean)
              .join(" ")
          : t("otmetka_snyata"),
      });
      setNonce((n) => n + 1);
      onChange();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const images = (view?.entries || []).filter((e) => e.thumb);
  return (
    <>
      <Notice>{ui.tree.fromIndex}</Notice>
      {error && <Notice tone="error">{error}</Notice>}

      <section className="panel">
        <h3>{ui.tree.marks}</h3>
        {view?.marks.length ? (
          <div className="mark-list">
            {view.marks.map((m) => (
              <div className="mark-row" key={m}>
                <Icon name="shield" size={18} />
                <button className="link" onClick={() => go(m)}>
                  <code className="path">{m}</code>
                </button>
                <Button
                  disabled={disabled || busy}
                  onClick={() => mark(m, false)}
                >
                  {ui.tree.unmark}
                </Button>
              </div>
            ))}
          </div>
        ) : (
          <p className="muted">{ui.tree.noMarks}</p>
        )}
      </section>

      {/* What the last mark did. Not inside the folder's own row: the rows
          are a fixed-height virtual list, and a sentence that lands there is
          cut off by the row below it. */}
      {note && (
        <Notice>
          <code className="path">{note.path}</code> {note.text}
        </Notice>
      )}

      <div className="toolbar tree-bar">
        <nav className="breadcrumb tree-crumbs" aria-label={ui.tree.root}>
          <button className="link" onClick={() => go("")}>
            {ui.tree.root}
          </button>
          {crumbs(path).map((c) => (
            <Fragment key={c.path}>
              <span aria-hidden="true">/</span>
              <button className="link" onClick={() => go(c.path)}>
                {c.name}
              </button>
            </Fragment>
          ))}
        </nav>
        {view?.parent !== null && view?.parent !== undefined && (
          <Button icon="arrow-left" onClick={() => go(view.parent!)}>
            {ui.tree.up}
          </Button>
        )}
      </div>

      <Resource r={r}>
        {view && (
          <>
            <section className="panel">
              <div className="section-heading">
                <div>
                  <h3>{path || ui.tree.root}</h3>
                  <span className="muted">
                    {number(view.files)} {t("faylov")} · {bytes(view.bytes)}
                  </span>
                </div>
                {/* Marking the root of the whole archive would call every
                    file an original and settle nothing, so it is offered on
                    real folders only. */}
                {!!path &&
                  (view.covered ? (
                    <div className="folder-note">
                      <span className="badge manual">{ui.tree.here}</span>
                      {/* The rule itself is spelled out over the folder list
                          below; here it only has to say which folder this
                          one's answer comes from. */}
                      {!view.marked && (
                        <p className="muted">
                          {t("vnutri_otmechennoy_papki", view.covered)}
                        </p>
                      )}
                      {view.marked && (
                        <Button
                          disabled={disabled || busy}
                          onClick={() => mark(path, false)}
                        >
                          {ui.tree.unmark}
                        </Button>
                      )}
                    </div>
                  ) : (
                    <Button
                      kind="primary"
                      icon="shield"
                      disabled={disabled || busy}
                      onClick={() => mark(path, true)}
                    >
                      {ui.tree.mark}
                    </Button>
                  ))}
              </div>
              {busy && <Loading />}
            </section>

            <section className="panel">
              <div className="section-heading">
                <div>
                  <h3>{ui.tree.folders}</h3>
                  {/* Said once, above the rows, rather than as a tooltip on
                      every one of them: it is the same sentence each time,
                      and it is what the button on each row means. */}
                  <p className="muted">{ui.tree.markHelp}</p>
                </div>
              </div>
              {view.directories.length ? (
                <VirtualList
                  items={view.directories}
                  rowHeight={78}
                  height={420}
                  resetKey={view.path}
                  render={(d) => (
                    <div className={`tree-row ${d.covered ? "marked" : ""}`}>
                      <button
                        className="tree-open"
                        title={d.path}
                        onClick={() => go(d.path)}
                      >
                        <Icon
                          name={d.covered ? "shield" : "folder"}
                          size={19}
                        />
                        <div>
                          <strong>{d.name}</strong>
                          <span className="muted">
                            {number(d.files)} {t("faylov")} · {bytes(d.bytes)}
                          </span>
                        </div>
                      </button>
                      {d.covered && !d.marked ? (
                        <span className="badge">{ui.tree.original}</span>
                      ) : (
                        <Button
                          kind={d.marked ? "selected" : ""}
                          disabled={disabled || busy}
                          onClick={() => mark(d.path, !d.marked)}
                        >
                          {d.marked ? ui.tree.unmark : ui.tree.mark}
                        </Button>
                      )}
                    </div>
                  )}
                />
              ) : (
                <p className="muted">{ui.tree.noFolders}</p>
              )}
            </section>

            <section className="panel">
              <div className="section-heading">
                <h3>
                  {number(view.here)} {t("faylov")}
                </h3>
                {view.shown < view.here && (
                  <span className="muted">
                    {t(
                      "pokazany_pervye_iz",
                      number(view.shown),
                      number(view.here),
                    )}
                  </span>
                )}
              </div>
              {view.entries.length ? (
                <div className="photo-grid">
                  {view.entries.map((e) => (
                    <article
                      className={`photo-card ${e.original ? "original" : ""}`}
                      key={e.file_id}
                    >
                      <Thumb
                        thumb={e.thumb}
                        name={e.name}
                        onClick={() =>
                          setViewing(
                            Math.max(
                              0,
                              images.findIndex((i) => i.file_id === e.file_id),
                            ),
                          )
                        }
                      />
                      <strong title={e.path}>{e.name}</strong>
                      <span className="muted">
                        {bytes(e.size)}
                        {e.width && e.height ? ` · ${e.width}×${e.height}` : ""}
                      </span>
                      <span className="tree-badges">
                        {e.original && (
                          <span className="badge manual">
                            {ui.tree.original}
                          </span>
                        )}
                        {e.is_keeper && (
                          <span className="badge">{ui.keeper}</span>
                        )}
                        {e.role_label && e.members > 1 && (
                          <span className="badge">
                            {e.role_label} ·{" "}
                            {t("versiy_v_gruppe", number(e.members))}
                          </span>
                        )}
                        {e.skipped_reason && (
                          <span className="badge warning">
                            {e.skipped_reason}
                          </span>
                        )}
                      </span>
                    </article>
                  ))}
                </div>
              ) : (
                <Empty title={ui.tree.noFiles} icon="image" />
              )}
            </section>
          </>
        )}
      </Resource>

      <section className="panel">
        <div className="section-heading">
          <div>
            <h3>{ui.tree.quarantine}</h3>
            <p className="muted">{ui.tree.quarantineHelp}</p>
          </div>
        </div>
        {view?.marks.length ? (
          <Review
            kind="plan-apply"
            params={{ roles: ["copy"], originals: true }}
            disabled={disabled}
            start={start}
            refresh={revision + nonce}
          />
        ) : (
          <p className="muted">{ui.tree.quarantineNoMarks}</p>
        )}
      </section>

      {viewing !== null && !!images.length && (
        <ImageViewer
          images={images}
          start={viewing}
          onClose={() => setViewing(null)}
        />
      )}
    </>
  );
}
