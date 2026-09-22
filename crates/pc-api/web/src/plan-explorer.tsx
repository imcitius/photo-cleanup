import { useEffect, useMemo, useRef, useState } from "react";
import { ReviewPhoto } from "./review-photo";
import { useDebounce } from "./api";
import { Button, Empty, Icon, Thumb } from "./components";
import { ImageViewer, type ImageRef } from "./curation";
import { basename, bytes, number, t, ui } from "./i18n";
import type { PlanItem, Preview } from "./types";

type Outcome = "move" | "stay" | "refusal";
type Entry = {
  key: string;
  path: string;
  outcome: Outcome;
  item?: PlanItem;
  why?: string;
  id?: number;
  thumb?: string | null;
};
const slashPath = (path: string) =>
  /^[A-Za-z]:[\\/]|^\\\\/.test(path) ? path.replaceAll("\\", "/") : path;
const folderOf = (path: string) => {
  const value = slashPath(path);
  const at = value.lastIndexOf("/");
  return at < 0 ? "" : value.slice(0, at) || "/";
};
const normalized = (text: string) =>
  text.normalize("NFKC").toLocaleLowerCase().replaceAll("\\", "/");

// A missing character or one mistyped character is useful for camera names;
// exact path fragments always rank ahead of approximate matches.
function near(a: string, b: string) {
  if (Math.abs(a.length - b.length) > 1) return false;
  let i = 0,
    j = 0,
    edits = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      i++;
      j++;
      continue;
    }
    if (++edits > 1) return false;
    if (a.length <= b.length) j++;
    if (a.length >= b.length) i++;
  }
  return edits + (a.length - i) + (b.length - j) <= 1;
}
function searchScore(text: string, query: string) {
  const haystack = normalized(text),
    terms = normalized(query).trim().split(/\s+/).filter(Boolean);
  let score = 0;
  for (const term of terms) {
    if (haystack.includes(term)) continue;
    const words = haystack.split(/[^\p{L}\p{N}]+/u);
    if (term.length >= 4 && words.some((word) => near(term, word))) {
      score += 1;
      continue;
    }
    let at = 0;
    // Abbreviations match within a path component, never across unrelated folders.
    if (
      term.length >= 3 &&
      haystack.split("/").some((part) => {
        at = 0;
        for (const c of part) if (c === term[at]) at++;
        return at === term.length;
      })
    ) {
      score += 2;
      continue;
    }
    return -1;
  }
  return score;
}
const label = (outcome: Outcome) =>
  outcome === "move"
    ? t("pe_move")
    : outcome === "stay"
      ? t("pe_stay")
      : t("pe_refusal");

export function PlanExplorer({ plan }: { plan: Preview }) {
  const [search, setSearch] = useState(""),
    [outcome, setOutcome] = useState("all"),
    [folder, setFolder] = useState(""),
    [folderPage, setFolderPage] = useState(0),
    [page, setPage] = useState(0),
    [selected, setSelected] = useState<string | null>(null),
    [view, setView] = useState<ImageRef[] | null>(null);
  const listRef = useRef<HTMLDivElement>(null),
    detailRef = useRef<HTMLElement>(null);
  const query = useDebounce(search, 150);
  const entries = useMemo(() => {
    const result: Entry[] = plan.items.map((item, i) => ({
      key: `m${i}`,
      path: item.path,
      outcome: "move",
      item,
      id: item.file_id,
      thumb: item.thumb,
    }));
    // Sidecars are real operations too; searching for an .xmp must explain
    // its destination rather than claim it is absent from the plan.
    for (const [i, item] of plan.items.entries()) {
      for (const [j, companion] of (item.companions || []).entries()) {
        result.push({
          key: `c${i}-${j}`,
          path: companion.path,
          outcome: "move",
          item: {
            ...companion,
            file_count: 1,
            reason: t("pe_companion_reason", basename(item.path)),
          },
        });
      }
    }
    const known = new Set(result.map((e) => e.path));
    for (const item of plan.items) {
      if (item.keeper_path && !known.has(item.keeper_path)) {
        known.add(item.keeper_path);
        result.push({
          key: `s${item.keeper_path}`,
          path: item.keeper_path,
          outcome: "stay",
          id: item.keeper_id,
          thumb: item.keeper_thumb,
        });
      }
    }
    plan.refusals.forEach((r, i) =>
      result.push({
        key: `r${i}`,
        path: r.path,
        why: r.why,
        outcome: "refusal",
        id: r.file_id,
        thumb: r.thumb,
      }),
    );
    return result;
  }, [plan]);
  const searched = useMemo(
    () =>
      entries
        .map((entry) => ({
          entry,
          score: searchScore(
            entry.path +
              " " +
              (entry.item?.dst || "") +
              " " +
              (entry.item?.keeper_path || ""),
            query,
          ),
        }))
        .filter((e) => e.score >= 0)
        .sort((a, b) => a.score - b.score)
        .map((e) => e.entry),
    [entries, query],
  );
  const inFolder = (path: string) =>
    !folder || slashPath(path).startsWith(folder.replace(/\/$/, "") + "/");
  const scoped = searched.filter((e) => inFolder(e.path));
  const filtered = scoped.filter(
    (e) => outcome === "all" || e.outcome === outcome,
  );
  const at = Math.min(page, Math.max(0, Math.ceil(filtered.length / 40) - 1));
  const shown = filtered.slice(at * 40, at * 40 + 40);
  const current = filtered.find((e) => e.key === selected) || shown[0];
  useEffect(() => {
    if (listRef.current) listRef.current.scrollTop = 0;
  }, [at, query, folder, outcome]);
  useEffect(() => {
    if (detailRef.current) detailRef.current.scrollTop = 0;
  }, [current?.key]);
  const folders = useMemo(() => {
    const counts = new Map<string, number>();
    for (const e of searched) {
      if (!inFolder(e.path)) continue;
      const dir = folderOf(e.path);
      const parent = folder.replace(/\/$/, "");
      if (dir === parent) continue;
      const start = parent
        ? parent.length + 1
        : dir.match(/^\/+/)?.[0].length || 0;
      const end = dir.indexOf("/", start);
      const child = dir.slice(0, end < 0 ? undefined : end);
      counts.set(child, (counts.get(child) || 0) + 1);
    }
    return [...counts].sort(([a], [b]) => a.localeCompare(b));
  }, [searched, folder]);
  const selectFolder = (value: string) => {
    setFolder(value);
    setFolderPage(0);
    setPage(0);
    setSelected(null);
  };
  const folderAt = Math.min(
    folderPage,
    Math.max(0, Math.ceil(folders.length / 100) - 1),
  );
  const images: ImageRef[] = current?.id
    ? [
        {
          file_id: current.id,
          name: basename(current.path),
          thumb: current.thumb,
        },
      ]
    : [];
  if (current?.item?.keeper_id && current.item.keeper_path)
    images.push({
      file_id: current.item.keeper_id,
      name: basename(current.item.keeper_path),
      thumb: current.item.keeper_thumb,
    });
  return (
    <section className="plan-explorer" aria-label={t("pe_title")}>
      <div className="explorer-toolbar">
        <label className="search-field">
          <Icon name="search" />
          <input
            type="search"
            aria-label={t("pe_search")}
            placeholder={t("pe_search")}
            value={search}
            onChange={(e) => {
              setSearch(e.target.value);
              setFolderPage(0);
              setPage(0);
              setSelected(null);
            }}
          />
        </label>
        <span className="muted">{t("pe_search_hint")}</span>
      </div>
      <div className="explorer-tabs" aria-label={t("pe_outcome")}>
        {(["all", "move", "stay", "refusal"] as const).map((value) => (
          <Button
            key={value}
            kind={outcome === value ? "selected" : ""}
            aria-pressed={outcome === value}
            onClick={() => {
              setOutcome(value);
              setPage(0);
              setSelected(null);
            }}
          >
            {value === "all" ? t("pe_all") : label(value)}{" "}
            <span>
              {number(
                value === "all"
                  ? scoped.length
                  : scoped.filter((e) => e.outcome === value).length,
              )}
            </span>
          </Button>
        ))}
      </div>
      <p className="muted explorer-scope">{t("pe_scope")}</p>
      <div className="explorer-layout">
        <aside className="explorer-folders" aria-label={t("pe_folders")}>
          <h3>{t("pe_folders")}</h3>
          <Button onClick={() => selectFolder("")} disabled={!folder}>
            {t("pe_all_folders")}
          </Button>
          {folder && (
            <>
              <Button
                icon="arrow-left"
                onClick={() =>
                  selectFolder(
                    folderOf(folder) === folder ? "" : folderOf(folder),
                  )
                }
              >
                {t("pe_up")}
              </Button>
              <code className="explorer-path">{folder}</code>
            </>
          )}
          <div className="folder-children" tabIndex={0}>
            {folders
              .slice(folderAt * 100, (folderAt + 1) * 100)
              .map(([path, count]) => (
                <button
                  key={path}
                  onClick={() => selectFolder(path)}
                  title={path}
                >
                  <Icon name="folder" size={16} />
                  <span>{basename(path)}</span>
                  <small>{number(count)}</small>
                </button>
              ))}
          </div>
          {folders.length > 100 && (
            <div className="explorer-pagination">
              <Button
                icon="arrow-left"
                aria-label={t("pe_folder_prev")}
                disabled={folderAt === 0}
                onClick={() => setFolderPage(folderAt - 1)}
              />
              <span>
                {folderAt + 1} / {Math.ceil(folders.length / 100)}
              </span>
              <Button
                icon="arrow"
                aria-label={t("pe_folder_next")}
                disabled={(folderAt + 1) * 100 >= folders.length}
                onClick={() => setFolderPage(folderAt + 1)}
              />
            </div>
          )}
          <small className="muted">{t("pe_folder_scope")}</small>
        </aside>
        <div className="explorer-results">
          <div className="section-heading">
            <strong>{t("pe_results", number(filtered.length))}</strong>
            <span className="muted">
              {filtered.length
                ? `${at * 40 + 1}–${Math.min((at + 1) * 40, filtered.length)}`
                : "0"}
            </span>
          </div>
          <div
            ref={listRef}
            className="explorer-list"
            tabIndex={0}
            aria-label={t("pe_results_list")}
          >
            {!shown.length ? (
              <Empty
                title={ui.noResults}
                action={<a href="#tree">{ui.pages.tree}</a>}
              >
                {t("pe_no_results")}
              </Empty>
            ) : (
              shown.map((e) => (
                <button
                  className={`explorer-row ${e.key.startsWith("m") ? "plan-row" : ""} ${e.key === current?.key ? "active" : ""}`}
                  key={e.key}
                  aria-pressed={e.key === current?.key}
                  onClick={() => setSelected(e.key)}
                >
                  <Thumb thumb={e.thumb} name="" />
                  <span>
                    <strong>{basename(e.path)}</strong>
                    <small title={e.path}>{folderOf(e.path)}</small>
                    <span
                      className={`badge ${e.outcome === "refusal" ? "warning" : ""}`}
                    >
                      {label(e.outcome)}
                    </span>
                  </span>
                </button>
              ))
            )}
          </div>
          <div className="explorer-pagination">
            <Button
              icon="arrow-left"
              aria-label={t("pe_prev")}
              disabled={at === 0}
              onClick={() => {
                setPage(at - 1);
                setSelected(null);
              }}
            />
            {at + 1} / {Math.max(1, Math.ceil(filtered.length / 40))}
            <Button
              icon="arrow"
              aria-label={t("pe_next")}
              disabled={(at + 1) * 40 >= filtered.length}
              onClick={() => {
                setPage(at + 1);
                setSelected(null);
              }}
            />
          </div>
        </div>
        <section
          ref={detailRef}
          className="explorer-detail"
          aria-label={t("pe_detail")}
        >
          {current ? (
            <>
              <div className="section-heading">
                <h3>{basename(current.path)}</h3>
                <span className="badge">{label(current.outcome)}</span>
              </div>
              {!!images.length && (
                <>
                  <div
                    className={`explorer-photos ${images.length === 2 ? "pair" : ""}`}
                  >
                    {images.map((image, i) => (
                      <figure key={image.file_id}>
                        <button
                          onClick={() => setView(images)}
                          aria-label={t("pe_open", image.name)}
                        >
                          <ReviewPhoto
                            key={image.file_id}
                            id={image.file_id}
                            name={image.name}
                          />
                        </button>
                        <figcaption>
                          {i === 1 ? ui.willStay : label(current.outcome)}
                        </figcaption>
                      </figure>
                    ))}
                  </div>
                  <Button onClick={() => setView(images)}>
                    {images.length === 2 ? ui.compare : t("pe_full")}
                  </Button>
                </>
              )}
              <dl className="explorer-facts">
                <div>
                  <dt>{t("pe_source")}</dt>
                  <dd>
                    <code>{current.path}</code>
                  </dd>
                </div>
                {current.item && (
                  <>
                    <div>
                      <dt>{t("pe_destination")}</dt>
                      <dd>
                        <code>{current.item.dst}</code>
                      </dd>
                    </div>
                    <div>
                      <dt>{t("pe_size")}</dt>
                      <dd>{bytes(current.item.size)}</dd>
                    </div>
                  </>
                )}
                {current.item?.keeper_path && (
                  <div>
                    <dt>{ui.willStay}</dt>
                    <dd>
                      <code>{current.item.keeper_path}</code>
                    </dd>
                  </div>
                )}
                <div>
                  <dt>{t("pe_reason")}</dt>
                  <dd>
                    {current.why || current.item?.reason || t("pe_kept_reason")}
                  </dd>
                </div>
              </dl>
              {!!current.item?.companions?.length && (
                <details>
                  <summary>
                    {t("sputniki")}
                    {current.item.companions.length}
                  </summary>
                  {current.item.companions.map((c) => (
                    <div className="explorer-companion" key={c.path}>
                      <code>{c.path}</code>
                      <Icon name="arrow" size={14} />
                      <code>{c.dst}</code>
                    </div>
                  ))}
                </details>
              )}
            </>
          ) : (
            <p className="muted">{t("pe_select")}</p>
          )}
        </section>
      </div>
      {view && <ImageViewer images={view} onClose={() => setView(null)} />}
    </section>
  );
}
