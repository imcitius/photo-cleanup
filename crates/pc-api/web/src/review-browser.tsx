import { useEffect, useMemo, useRef, useState } from "react";
import {
  Button,
  ErrorBox,
  Icon,
  Loading,
  Thumb,
  VirtualList,
} from "./components";
import { number, t, ui } from "./i18n";
import type { useReviewQueue } from "./use-review-queue";

type Queue = ReturnType<typeof useReviewQueue>;
export function ReviewBrowser({ q }: { q: Queue }) {
  const [open, setOpen] = useState<Set<string>>(new Set());
  const list = useRef<HTMLDivElement>(null);
  const folders = q.tree.data?.folders || [];
  const children = useMemo(
    () => new Set(folders.map((f) => f.path.split("/").slice(0, -1).join("/"))),
    [folders],
  );
  const visible = folders.filter((f) => {
    const parts = f.path.split("/");
    return parts
      .slice(0, -1)
      .every((_, i) => open.has(parts.slice(0, i + 1).join("/")));
  });
  const toggle = (path: string) =>
    setOpen((old) => {
      const next = new Set(old);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  useEffect(() => {
    const node = list.current;
    const selected = node?.children[q.at] as HTMLElement | undefined;
    if (!node || !selected) return;
    // Scroll this pane only, and only when keyboard navigation leaves it.
    // Selecting a visible group must never shift the page or reorder rows.
    if (selected.offsetTop < node.scrollTop)
      node.scrollTop = selected.offsetTop;
    else if (
      selected.offsetTop + selected.offsetHeight >
      node.scrollTop + node.clientHeight
    )
      node.scrollTop =
        selected.offsetTop + selected.offsetHeight - node.clientHeight;
  }, [q.current?.id, q.offset, q.at]);
  return (
    <aside className="review-browser" aria-label={t("rq_browse")}>
      <nav className="review-folders" aria-label={t("rq_folder_tree")}>
        <h3>{t("rq_folder_tree")}</h3>
        <Button
          aria-pressed={!q.folder}
          disabled={q.busy}
          onClick={() => q.filter("", "folder")}
        >
          <Icon name="folder" />
          {t("pe_all_folders")}
        </Button>
        {q.tree.error ? (
          <ErrorBox message={q.tree.error} retry={q.tree.reload} />
        ) : q.tree.loading && !q.tree.data ? (
          <Loading />
        ) : (
          <VirtualList
            items={visible}
            rowHeight={36}
            height={200}
            resetKey={`${q.search}/${q.queue}/${q.kind}`}
            render={(f) => (
              <div
                className="review-folder-row"
                style={{
                  paddingLeft: Math.min(5, f.path.split("/").length - 1) * 12,
                }}
              >
                {children.has(f.path) ? (
                  <button
                    className="folder-toggle"
                    aria-label={t("rq_expand_folder", f.path)}
                    aria-expanded={open.has(f.path)}
                    onClick={() => toggle(f.path)}
                  >
                    {open.has(f.path) ? "▾" : "▸"}
                  </button>
                ) : (
                  <span className="folder-spacer" />
                )}
                <button
                  className="review-folder"
                  title={f.path}
                  aria-pressed={q.folder === f.path}
                  disabled={q.busy}
                  onClick={() => {
                    q.filter(f.path, "folder");
                    setOpen((old) => new Set([...old, f.path]));
                  }}
                >
                  <span>{f.path.split("/").at(-1)}</span>
                  <small>{number(f.groups)}</small>
                </button>
              </div>
            )}
          />
        )}
        {q.folder && <p className="review-folder-path">{q.folder}</p>}
      </nav>
      <div className="review-list-heading">
        <strong>{t("rq_queue")}</strong>
        <span>
          {number(q.total ? q.offset + 1 : 0)}–
          {number(q.offset + q.groups.length)} / {number(q.total)}
        </span>
      </div>
      <div
        className="review-group-list"
        ref={list}
        aria-label={t("rq_queue")}
        aria-busy={q.blocked}
      >
        {q.groups.map((g, i) => (
          <button
            className="queue-group"
            key={g.id}
            aria-pressed={q.current?.id === g.id}
            disabled={q.blocked}
            onClick={() => q.select(i)}
          >
            <Thumb
              thumb={
                g.members.find((m) => m.is_keeper)?.thumb || g.members[0]?.thumb
              }
              name=""
            />
            <span>
              <strong>
                {g.members.find((m) => m.is_keeper)?.name || g.members[0]?.name}
              </strong>
              <small>
                {number(q.offset + i + 1)} · {g.members.length} {t("faylov")} ·{" "}
                {g.exact ? t("rq_exact") : t("rq_versions")}
              </small>
              <small>
                {g.review_state === "keep"
                  ? t("rq_kept")
                  : g.review_state === "defer"
                    ? t("rq_deferred")
                    : g.review_state === "plan"
                      ? t("rq_planned")
                      : g.decision_source === "folder"
                        ? t("rq_source_folder")
                        : g.decision_source === "manual"
                          ? t("rq_source_manual")
                          : t("rq_pending")}
              </small>
            </span>
          </button>
        ))}
        {!q.groups.length && !q.r.loading && (
          <p className="muted">{ui.noResults}</p>
        )}
      </div>
      <div className="review-list-pages">
        <Button
          aria-label={t("rq_prev_page")}
          disabled={q.blocked || q.offset === 0}
          onClick={() => q.jump(Math.max(0, q.offset - 50))}
        >
          ← {t("rq_prev_page")}
        </Button>
        <Button
          aria-label={t("rq_next_page")}
          disabled={q.blocked || q.offset + q.groups.length >= q.total}
          onClick={() => q.jump(q.offset + 50)}
        >
          {t("rq_next_page")} →
        </Button>
      </div>
    </aside>
  );
}
