// The archive as its owner already knows it: a tree of folders, opened and
// closed, with the photographs in them.
//
// Every other screen here is a list of decisions the tool has prepared. This
// one exists for one sentence that cannot be said on any of them: "the
// originals are in here". Said once about a folder, it settles every group
// that folder has a hand in — and it keeps settling them, because the mark is
// stored and applied again after every rebuild.
//
// The tree is laid *across* the roots. On an array the photographs live on
// three filesystems and their owner sees one structure over the three, so
// `D/разобрано/даня/театр` is one node here even when it exists three times
// on disk. Which disks hold it is what the node says; the absolute paths are
// one level down, in the folder's own panel, for the times when one disk has
// to be singled out.
import { useCallback, useEffect, useState } from "react";
import { api, post, useResource } from "./api";
import {
  Button,
  Empty,
  Icon,
  Loading,
  Notice,
  Resource,
  Thumb,
} from "./components";
import { ImageViewer } from "./curation";
import { bytes, number, t, ui } from "./i18n";
import type { TreeFiles, TreeNode, TreeRoot, TreeView } from "./types";
import { openPlan } from "./plan-source";

/** Where the last visit left off, so a reload does not start at the top. */
const LAST = "pc-tree-path";

/** The path split into the pieces a breadcrumb can be built from. */
function crumbs(path: string) {
  const out: { name: string; path: string }[] = [];
  let at = 0;
  while (at < path.length) {
    const rest = path.slice(at);
    const lead = rest.length - rest.replace(/^[/\\]+/, "").length;
    const body = rest.slice(lead);
    if (!body) break;
    const next = body.search(/[/\\]/);
    const end = next === -1 ? path.length : at + lead + next;
    out.push({ name: path.slice(at + lead, end), path: path.slice(0, end) });
    at = end;
  }
  return out;
}

/** Put a freshly fetched subtree in place of the stub that stood for it. */
function graft(node: TreeNode, path: string, fresh: TreeNode): TreeNode {
  if (node.path === path) return fresh;
  if (!node.children) return node;
  return { ...node, children: node.children.map((c) => graft(c, path, fresh)) };
}

function find(node: TreeNode | null, path: string): TreeNode | null {
  if (!node) return null;
  if (node.path === path) return node;
  for (const child of node.children || []) {
    const hit = find(child, path);
    if (hit) return hit;
  }
  return null;
}

/** The open folders, in the order a tree draws them. */
function rows(node: TreeNode | null, open: Set<string>, depth = 0) {
  const out: { node: TreeNode; depth: number }[] = [];
  for (const child of node?.children || []) {
    out.push({ node: child, depth });
    if (open.has(child.path)) out.push(...rows(child, open, depth + 1));
  }
  return out;
}

export function Tree({
  revision,
  disabled,
  onChange,
}: {
  revision: number;
  disabled: boolean;
  onChange: () => void;
}) {
  const [tree, setTree] = useState<TreeNode | null>(null),
    [view, setView] = useState<TreeView | null>(null),
    [open, setOpen] = useState<Set<string>>(new Set()),
    [selected, setSelected] = useState(() => localStorage.getItem(LAST) || ""),
    [nonce, setNonce] = useState(0),
    [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    // What the last mark did. The numbers are the whole answer to "did that
    // do anything?", and they are gone the moment the tree redraws otherwise.
    [note, setNote] = useState<{ path: string; text: string } | null>(null),
    [viewing, setViewing] = useState<number | null>(null);

  useEffect(() => {
    let cancelled = false;
    api<TreeView>("/tree?depth=2")
      .then((top) => {
        if (cancelled) return;
        setView(top);
        setTree(top.node);
        setError("");
      })
      .catch((e) => !cancelled && setError((e as Error).message));
    return () => {
      cancelled = true;
    };
  }, [revision, nonce]);

  useEffect(() => {
    try {
      localStorage.setItem(LAST, selected);
    } catch {
      // A private window forgets where we were. Nothing else breaks.
    }
  }, [selected]);

  const load = useCallback(async (path: string) => {
    const fetched = await api<TreeView>(
      `/tree?depth=2&path=${encodeURIComponent(path)}`,
    );
    setTree((old) => (old ? graft(old, path, fetched.node) : fetched.node));
  }, []);

  const toggle = async (node: TreeNode) => {
    const next = new Set(open);
    if (next.has(node.path)) {
      next.delete(node.path);
      setOpen(next);
      return;
    }
    next.add(node.path);
    setOpen(next);
    // Two levels arrive at a time, which is what a tree needs to draw itself:
    // the children, and whether each of them has children of its own. Only a
    // folder opened past that depth costs a request.
    if (node.children === null) {
      try {
        await load(node.path);
      } catch (e) {
        setError((e as Error).message);
      }
    }
  };

  // Walk down to the folder the last visit left off at, opening each step:
  // coming back to a page that has forgotten where you were is the same as
  // not remembering at all.
  const arrived = !!tree;
  useEffect(() => {
    if (!arrived || !selected) return;
    const wanted = crumbs(selected).map((c) => c.path);
    let cancelled = false;
    (async () => {
      for (const path of wanted) {
        if (cancelled) return;
        try {
          await load(path);
        } catch {
          return;
        }
      }
      if (!cancelled) setOpen((o) => new Set([...o, ...wanted]));
    })();
    return () => {
      cancelled = true;
    };
    // Driven by the tree arriving, not by every press afterwards.
  }, [arrived, nonce]); // eslint-disable-line react-hooks/exhaustive-deps

  const files = useResource<TreeFiles>(
    `/tree/files?path=${encodeURIComponent(selected)}`,
    revision + nonce,
  );

  const mark = async (
    path: string,
    scope: "every-root" | "absolute",
    marked: boolean,
  ) => {
    setBusy(true);
    setError("");
    try {
      const report = await post<{
        groups: number;
        moved: number;
        untouched: number;
      }>("/originals", { path, scope, marked });
      setNote({
        path,
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
      // The whole tree is fetched again: a mark changes what counts as an
      // original far outside the folder it was pressed on, and a page that
      // redrew only the row under the cursor would be lying about the rest.
      setNonce((n) => n + 1);
      onChange();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const current = find(tree, selected);
  const images = (files.data?.entries || []).filter((e) => e.thumb);
  const listed = rows(tree, open);

  return (
    <>
      <Notice>{ui.tree.fromIndex}</Notice>
      {error && <Notice tone="error">{error}</Notice>}

      {/* What the tree is laid over. On one root this says nothing anybody
          needs; on three it is the difference between one structure and
          three, which is the whole reason the page merges them. */}
      {view?.merged && (
        <section className="panel">
          <div className="section-heading">
            <div>
              <h3>{ui.tree.disks}</h3>
              <p className="muted">{ui.tree.disksHelp}</p>
            </div>
            <strong>{bytes(view.bytes)}</strong>
          </div>
          <div className="disk-row">
            {view.roots.map((r) => (
              <span className="badge" key={r.path} title={r.path}>
                <Icon name="layers" size={13} /> {r.label} · {number(r.files)}
              </span>
            ))}
          </div>
        </section>
      )}

      <section className="panel">
        <h3>{ui.tree.marks}</h3>
        {view?.marks.length ? (
          <div className="mark-list">
            {view.marks.map((m) => (
              <div className="mark-row" key={`${m.scope}:${m.path}`}>
                <Icon name="shield" size={18} />
                <button
                  className="link"
                  disabled={m.scope !== "every-root"}
                  onClick={() => setSelected(m.path)}
                >
                  <code className="path">{m.path}</code>
                </button>
                <span className="badge">
                  {m.scope === "every-root"
                    ? ui.tree.everyRoot
                    : ui.tree.oneDisk}
                </span>
                {!!m.files && (
                  <span className="muted">
                    {number(m.files)} {t("faylov")}
                  </span>
                )}
                <Button
                  disabled={disabled || busy}
                  onClick={() => mark(m.path, m.scope, false)}
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

      {note && (
        <Notice>
          <code className="path">{note.path}</code> {note.text}
        </Notice>
      )}

      <div className="tree-layout">
        <section className="panel tree-panel">
          <h3>{ui.tree.folders}</h3>
          {!tree ? (
            <Loading />
          ) : (
            <div className="tree-rows">
              <div className="tree-line">
                <button
                  className={`tree-row ${selected === "" ? "current" : ""}`}
                  onClick={() => setSelected("")}
                >
                  <span className="tree-twist" />
                  <Icon name="layers" size={17} />
                  <span className="tree-name">{ui.tree.root}</span>
                  <span className="muted">{number(view?.files || 0)}</span>
                </button>
              </div>
              {!listed.length && <p className="muted">{ui.tree.noFolders}</p>}
              {listed.map(({ node: n, depth }) => (
                <div
                  className={`tree-line ${n.covered ? "marked" : ""}`}
                  key={n.path}
                  data-path={n.path}
                  style={{ paddingLeft: depth * 17 }}
                >
                  <button
                    className={`tree-row ${selected === n.path ? "current" : ""}`}
                    onClick={() => setSelected(n.path)}
                    title={n.path}
                  >
                    <span
                      className={`tree-twist ${n.has_children ? "has" : ""} ${
                        open.has(n.path) ? "open" : ""
                      }`}
                      aria-hidden="true"
                      onClick={(e) => {
                        if (!n.has_children) return;
                        e.stopPropagation();
                        toggle(n);
                      }}
                    >
                      {n.has_children && <Icon name="arrow" size={13} />}
                    </span>
                    <Icon name={n.covered ? "shield" : "folder"} size={17} />
                    <span className="tree-name">{n.name}</span>
                    <span className="muted">{number(n.files)}</span>
                    {/* Which disks hold this folder, in the tree itself: on an
                        array that is half of what a folder is. */}
                    {/* Only when it says something. A folder that exists on
                        every disk is the ordinary case, and three chips
                        repeating that on every row crowd out the names. */}
                    {view?.merged && n.roots.length < view.roots.length && (
                      <span className="tree-disks">
                        {n.roots.map((r) => (
                          <span key={r.path}>{r.label}</span>
                        ))}
                      </span>
                    )}
                  </button>
                </div>
              ))}
            </div>
          )}
        </section>

        <section className="panel tree-detail">
          <div className="section-heading">
            <div>
              <nav className="breadcrumb tree-crumbs" aria-label={ui.tree.root}>
                <button className="link" onClick={() => setSelected("")}>
                  {ui.tree.root}
                </button>
                {crumbs(selected).map((c) => (
                  <span key={c.path}>
                    <span aria-hidden="true"> / </span>
                    <button
                      className="link"
                      onClick={() => setSelected(c.path)}
                    >
                      {c.name}
                    </button>
                  </span>
                ))}
              </nav>
              {current && (
                <span className="muted">
                  {number(current.files)} {t("faylov")} · {bytes(current.bytes)}
                </span>
              )}
            </div>
            {/* Marking the whole archive would call every file an original
                and settle nothing, so it is offered on real folders only. */}
            {!!selected &&
              (current?.covered ? (
                <div className="inline">
                  <span className="badge manual">
                    {current.marked
                      ? ui.tree.here
                      : t("vnutri_otmechennoy_papki", current.covered.path)}
                  </span>
                  {current.marked && (
                    <Button
                      disabled={disabled || busy}
                      onClick={() => mark(selected, "every-root", false)}
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
                  onClick={() => mark(selected, "every-root", true)}
                >
                  {ui.tree.mark}
                </Button>
              ))}
          </div>
          {!!selected && !current?.covered && (
            <p className="muted mark-help">{ui.tree.markHelp}</p>
          )}
          {busy && <Loading />}

          {/* One disk of the array, when the answer is about that disk. */}
          {view?.merged && !!selected && !!current?.roots.length && (
            <details className="per-disk">
              <summary>{ui.tree.perDisk}</summary>
              <p className="muted">{ui.tree.perDiskHelp}</p>
              {current.roots.map((r: TreeRoot) => (
                <div className="mark-row" key={r.path}>
                  <Icon name={r.covered ? "shield" : "layers"} size={16} />
                  <code className="path">{r.full}</code>
                  <span className="muted">
                    {number(r.files)} {t("faylov")} · {bytes(r.bytes)}
                  </span>
                  <Button
                    kind={r.marked ? "selected" : ""}
                    disabled={disabled || busy}
                    onClick={() => mark(r.full!, "absolute", !r.marked)}
                  >
                    {r.marked ? ui.tree.unmark : ui.tree.onlyThisDisk}
                  </Button>
                </div>
              ))}
            </details>
          )}

          <Resource r={files}>
            {files.data && (
              <>
                <div className="section-heading">
                  <h3>
                    {number(files.data.here)} {t("faylov")}
                  </h3>
                  {files.data.shown < files.data.here && (
                    <span className="muted">
                      {t(
                        "pokazany_pervye_iz",
                        number(files.data.shown),
                        number(files.data.here),
                      )}
                    </span>
                  )}
                </div>
                {files.data.entries.length ? (
                  <div className="photo-grid">
                    {files.data.entries.map((e) => (
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
                                images.findIndex(
                                  (i) => i.file_id === e.file_id,
                                ),
                              ),
                            )
                          }
                        />
                        <strong title={e.path}>{e.name}</strong>
                        <span className="muted">
                          {bytes(e.size)}
                          {e.width && e.height
                            ? ` · ${e.width}×${e.height}`
                            : ""}
                        </span>
                        <span className="tree-badges">
                          {view?.merged && (
                            <span className="badge">{e.root_label}</span>
                          )}
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
              </>
            )}
          </Resource>
        </section>
      </div>

      <section
        className="panel tree-plan-step"
        aria-label={t("tree_next_step")}
      >
        <div className="section-heading">
          <div>
            <h3>{t("tree_next_step")}</h3>
            <p className="muted">{t("tree_plan_help")}</p>
          </div>
          <Button
            kind="primary"
            icon="arrow"
            disabled={!view?.marks.length || busy}
            onClick={() => openPlan("originals")}
          >
            {t("tree_open_plan")}
          </Button>
        </div>
        <p className="muted">
          {view?.marks.length
            ? t("tree_plan_scope", number(view.marks.length))
            : t("tree_plan_empty")}
        </p>
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
