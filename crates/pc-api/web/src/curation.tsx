import { day, t } from "./i18n";
import { useEffect, useRef, useState } from "react";
import { api, post, useDebounce, useResource } from "./api";
import {
  Button,
  Empty,
  ErrorBox,
  FileDetails,
  Icon,
  Loading,
  Modal,
  Notice,
  Resource,
  Thumb,
  VirtualList,
} from "./components";
import { basename, bytes, number, roleHelp, ui, when } from "./i18n";
import type { Category, Family, Member, Preview, Series } from "./types";
import type { Start } from "./workflow";
export interface ImageRef {
  file_id: number;
  name: string;
  thumb?: string | null;
}
export function ImageViewer({
  images,
  start = 0,
  onClose,
}: {
  images: ImageRef[];
  /** Which frame to open on; the rest stay reachable with the arrows. */
  start?: number;
  onClose: () => void;
}) {
  const last = Math.max(0, images.length - 1);
  // Two frames are always a comparison — whether they arrived from the
  // compare button or are simply all a family has — so a pair opens paired.
  const paired = images.length === 2;
  const [at, setAt] = useState(paired ? 1 : Math.min(Math.max(start, 0), last)),
    [pinned, setPinned] = useState<number | null>(paired ? 0 : null),
    [overlay, setOverlay] = useState(false),
    [one, setOne] = useState(false),
    [opacity, setOpacity] = useState(50),
    [pan, setPan] = useState({ x: 0, y: 0 }),
    [error, setError] = useState("");
  const drag = useRef<{ x: number; y: number; px: number; py: number } | null>(
    null,
  );
  // Flipping between two frames in one spot is how a burst is read: the eye
  // cannot hold a detail while it travels across a page, so a difference of
  // a few pixels only shows when the frames swap in place.
  const step = (by: number) => {
    setAt((n) => Math.min(last, Math.max(0, n + by)));
    setError("");
  };
  useEffect(() => {
    const listener = (e: KeyboardEvent) => {
      if (e.key === "ArrowRight" || e.key === "ArrowDown") {
        e.preventDefault();
        step(1);
      }
      if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
        e.preventDefault();
        step(-1);
      }
      if (e.key.toLowerCase() === "c" || e.key.toLowerCase() === "с") {
        e.preventDefault();
        setPinned((p) => (p === null ? at : null));
      }
    };
    document.addEventListener("keydown", listener);
    return () => document.removeEventListener("keydown", listener);
  }, [at, last]);
  const current = images[at];
  const shown = pinned === null ? [current] : [images[pinned], current];
  const comparing = shown.length === 2;
  return (
    <Modal title={comparing ? ui.compare : current.name} wide onClose={onClose}>
      <div className="toolbar">
        {images.length > 1 && (
          <div className="inline frame-nav">
            <Button
              icon="arrow-left"
              aria-label={ui.previousFrame}
              disabled={at === 0}
              onClick={() => step(-1)}
            />
            <span className="muted">
              {at + 1} / {images.length}
            </span>
            <Button
              icon="arrow"
              aria-label={ui.nextFrame}
              disabled={at === last}
              onClick={() => step(1)}
            />
          </div>
        )}
        <Button
          kind={pinned !== null ? "selected" : ""}
          onClick={() => setPinned(pinned === null ? at : null)}
        >
          {pinned === null ? ui.pinForCompare : ui.unpin}
        </Button>
        {comparing && (
          <>
            <Button
              kind={!overlay ? "selected" : ""}
              onClick={() => setOverlay(false)}
            >
              {t("ryadom")}
            </Button>
            <Button
              kind={overlay ? "selected" : ""}
              onClick={() => setOverlay(true)}
            >
              {t("nalozhenie")}
            </Button>
          </>
        )}
        <Button
          kind={one ? "selected" : ""}
          onClick={() => {
            setOne(!one);
            setPan({ x: 0, y: 0 });
          }}
        >
          1:1
        </Button>
        <Button
          onClick={() => {
            setPan({ x: 0, y: 0 });
            setOne(false);
          }}
        >
          {t("vpisat")}
        </Button>
        {overlay && comparing && (
          <label className="inline">
            {t("prozrachnost")}
            <input
              type="range"
              min="0"
              max="100"
              value={opacity}
              onChange={(e) => setOpacity(+e.target.value)}
            />
            {opacity}%
          </label>
        )}
      </div>
      <p className="muted">{ui.viewerHelp}</p>
      {error && <ErrorBox message={error} />}
      <div
        className={`image-viewer ${overlay && comparing ? "overlay" : ""} ${one ? "one-to-one" : ""}`}
        onPointerDown={(e) => {
          if (one) {
            e.currentTarget.setPointerCapture(e.pointerId);
            drag.current = { x: e.clientX, y: e.clientY, px: pan.x, py: pan.y };
          }
        }}
        onPointerMove={(e) => {
          if (drag.current)
            setPan({
              x: drag.current.px + e.clientX - drag.current.x,
              y: drag.current.py + e.clientY - drag.current.y,
            });
        }}
        onPointerUp={() => {
          drag.current = null;
        }}
        onPointerCancel={() => {
          drag.current = null;
        }}
      >
        {shown.map((image, i) => (
          <figure
            key={`${i}-${image.file_id}`}
            style={
              overlay && comparing && i === 1
                ? { opacity: opacity / 100 }
                : undefined
            }
          >
            <img
              draggable={false}
              // Pixel for pixel only when the frame is being read that way:
              // the fitted view is a screen-sized render, which is a fraction
              // of the bytes and of the wait on a NAS.
              src={`/api/file/${image.file_id}/preview${one ? "?full=1" : ""}`}
              alt={image.name}
              style={{ transform: `translate(${pan.x}px, ${pan.y}px)` }}
              // An <img> cannot read why the server refused, and "could not
              // read the frame" alone leaves the question the user actually
              // has — is the file broken? — unanswered. So ask again and
              // show what the server says.
              onError={async () => {
                const fallback = t(
                  "ne_udalos_prochitat_polnyy_kadr_fayl",
                  image.name,
                  image.file_id,
                );
                try {
                  await api(`/file/${image.file_id}/preview`);
                  setError(fallback);
                } catch (e) {
                  setError(`${fallback} — ${(e as Error).message}`);
                }
              }}
            />
            <figcaption>
              {image.name}
              {comparing && i === 0 ? ` · ${ui.pinnedFrame}` : ""}
            </figcaption>
          </figure>
        ))}
      </div>
      {/* Where this frame is and what the camera wrote. Deciding between two
          versions means knowing which folder each came from, and that was the
          one thing the viewer did not say. */}
      <div className="viewer-details">
        {shown.map((image, i) => (
          <details key={`d-${i}-${image.file_id}`} open>
            <summary>
              {image.name}
              {comparing && i === 0 ? ` · ${ui.pinnedFrame}` : ""}
            </summary>
            <FileDetails fileId={image.file_id} />
          </details>
        ))}
      </div>
      {/* The next frames, fetched quietly, so flipping is instant rather than
          a wait in which the difference is forgotten. */}
      <div hidden>
        {[at - 1, at + 1]
          .filter((n) => n >= 0 && n <= last)
          .map((n) => (
            <img
              key={n}
              src={`/api/file/${images[n].file_id}/preview`}
              alt=""
            />
          ))}
      </div>
      {images.length > 1 && (
        <div className="viewer-strip">
          {images.map((image, n) => (
            <button
              key={image.file_id}
              className={n === at ? "current" : pinned === n ? "pinned" : ""}
              aria-label={image.name}
              aria-current={n === at}
              onClick={() => {
                setAt(n);
                setError("");
              }}
            >
              <img src={`/api/thumb/${image.thumb}`} alt="" loading="lazy" />
            </button>
          ))}
        </div>
      )}
    </Modal>
  );
}
export function Families({
  revision,
  disabled,
  onChange,
}: {
  revision: number;
  disabled: boolean;
  onChange: () => void;
}) {
  const [search, setSearch] = useState(""),
    [role, setRole] = useState(""),
    [disk, setDisk] = useState(""),
    [min, setMin] = useState(""),
    [sort, setSort] = useState("space"),
    [all, setAll] = useState(false),
    [scroll, setScroll] = useState(0),
    [total, setTotal] = useState(0),
    [cache, setCache] = useState<Map<number, Family>>(new Map()),
    [selected, setSelected] = useState<Family | null>(null),
    [selection, setSelection] = useState<number[]>([]),
    [view, setView] = useState<{ images: ImageRef[]; start: number } | null>(
      null,
    ),
    [error, setError] = useState(""),
    [busy, setBusy] = useState(false),
    [moved, setMoved] = useState<{ family: number; names: string[] } | null>(
      null,
    ),
    // What the last folder action said, kept beside the folder it was about:
    // an answer shown at the top of a page the user has scrolled away from
    // reads as nothing happening at all.
    [folderNote, setFolderNote] = useState<{
      dir: string;
      text: string;
      // The move this answer offers next, when there is one.
      move?: { token: string; files: number; bytes: number; scope: string };
    } | null>(null),
    [focused, setFocused] = useState<number | null>(null);
  const listRef = useRef<HTMLDivElement>(null),
    searchRef = useRef<HTMLInputElement>(null);
  // The list fills whatever height the group beside it takes, so it is
  // measured rather than fixed: a short pane would otherwise leave the list
  // ending in mid-air, and a tall one would cut the scrolling short.
  const [height, setHeight] = useState(600);
  const rowHeight = 92;
  useEffect(() => {
    const node = listRef.current;
    if (!node) return;
    const observer = new ResizeObserver(([entry]) =>
      setHeight(entry.contentRect.height),
    );
    observer.observe(node);
    return () => observer.disconnect();
  }, []);
  const query = useDebounce(
    new URLSearchParams({
      search,
      role,
      disk,
      min_bytes: String(Math.max(0, +min) * 1024 * 1024),
      sort,
      all: String(all),
    }).toString(),
  );
  const first = Math.max(0, Math.floor(scroll / rowHeight) - 3),
    page = Math.floor(first / 50) * 50;
  const r = useResource<{ total: number; families: Family[] }>(
    `/families?${query}&limit=100&offset=${page}`,
    revision,
  );
  // A new search is a new list, so it starts from the top with nothing
  // selected.
  useEffect(() => {
    setScroll(0);
    setCache(new Map());
    setSelected(null);
    setTotal(0);
    if (listRef.current) listRef.current.scrollTop = 0;
  }, [query]);
  // A finished job is the same list with one group fewer. Only the rows are
  // stale: keeping the scroll, the selection and the count means the page
  // does not blink back to the top and rebuild itself after every press.
  useEffect(() => {
    setCache(new Map());
  }, [revision]);
  useEffect(() => {
    if (r.data) {
      setTotal(r.data.total);
      setCache((old) => {
        const next = new Map(old);
        r.data!.families.forEach((f, i) => next.set(page + i, f));
        if (next.size > 400) {
          for (const k of next.keys())
            if (k < page - 100 || k > page + 200) next.delete(k);
        }
        return next;
      });
      setSelected((s) => s || r.data!.families[0] || null);
    }
  }, [r.data]);
  useEffect(() => {
    setSelection([]);
    setFocused(selected?.members[0]?.file_id || null);
  }, [selected?.id]);
  const keeper = async (file: number) => {
    if (!selected || busy || disabled) return;
    const before = selected;
    const next = {
      ...selected,
      members: selected.members.map((m) => ({
        ...m,
        is_keeper: m.file_id === file,
      })),
    };
    setSelected(next);
    setBusy(true);
    setError("");
    try {
      await post(`/families/${selected.id}/keeper`, { file_id: file });
      setCache(
        (old) =>
          new Map([...old].map(([k, f]) => [k, f.id === next.id ? next : f])),
      );
    } catch (e) {
      setSelected(before);
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  useEffect(() => {
    const listener = (e: KeyboardEvent) => {
      if (
        document.querySelector("dialog[open]") ||
        (e.target instanceof HTMLElement &&
          e.target.closest("input,select,textarea"))
      )
        return;
      if (
        (e.key === "Enter" || e.code === "Space") &&
        e.target instanceof HTMLElement &&
        e.target.closest("button,a") &&
        !e.target.closest(".family-list-item")
      )
        return;
      if (e.key === "/") {
        e.preventDefault();
        searchRef.current?.focus();
      }
      if (e.key === "j" || e.key === "k") {
        e.preventDefault();
        const index =
          [...cache].find(([, f]) => f.id === selected?.id)?.[0] ?? 0;
        const next = Math.min(
          total - 1,
          Math.max(0, index + (e.key === "j" ? 1 : -1)),
        );
        if (cache.has(next)) setSelected(cache.get(next)!);
        listRef.current?.scrollTo({ top: Math.max(0, (next - 2) * rowHeight) });
      }
      if (e.code === "Space" && focused) {
        e.preventDefault();
        keeper(focused);
      }
      if (e.key === "Enter" && selected) {
        const m =
          selected.members.find((m) => m.file_id === focused) ||
          selected.members[0];
        if (m) {
          e.preventDefault();
          setView({
            images: selected.members,
            start: selected.members.indexOf(m),
          });
        }
      }
    };
    document.addEventListener("keydown", listener);
    return () => document.removeEventListener("keydown", listener);
  }, [cache, selected, focused, total, busy, disabled]);
  // One press. The group on screen already lists its files and says how much
  // is in copies, so a confirmation would only be asking the same question
  // twice — and this is a move to quarantine, which the journal undoes.
  //
  // The plan is still fetched first and the run still carries its token: if
  // the group changed between the two calls the server refuses, which is the
  // check that matters. It just does not need the user's hands for it.
  const moveGroup = async (family: Family) => {
    setBusy(true);
    setError("");
    try {
      const preview = await post<Preview>("/preview", {
        kind: "plan-apply",
        params: { roles: ["copy"], family_id: family.id },
      });
      if (!preview.items.length) {
        setError(ui.groupApplyNothing);
        return;
      }
      await post("/jobs", {
        kind: "plan-apply",
        params: { roles: ["copy"], family_id: family.id },
        plan_token: preview.token,
      });
      setMoved({
        family: family.id,
        names: preview.items.map((i) => basename(i.path)),
      });
      // The group leaves the list on its own — its copies are no longer in
      // the archive — so the next one is opened in its place. Going through
      // ten thousand groups is one press each, not a press and a hunt for
      // where the list has jumped to.
      const index = [...cache].find(([, f]) => f.id === family.id)?.[0];
      const next = index === undefined ? null : cache.get(index + 1) || null;
      setSelected(next);
      onChange();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  // Ten thousand groups, one press each, is still ten thousand presses. An
  // archive usually has one folder the photographs were worked in; told
  // which, every group that has a file there keeps that file.
  const preferFolder = async (dir: string) => {
    setBusy(true);
    setError("");
    try {
      const r = await post<{ groups: number }>("/keepers/prefer-folder", {
        dir,
      });
      // Naming the folder is half a decision; what follows from it is taking
      // away what duplicates those groups. Offered here, with its numbers,
      // rather than left for the user to find.
      const next = await plannedMove("keeper_folder", dir);
      setFolderNote({
        dir,
        text: t("papka_teper_hranimaya", number(r.groups), dir),
        move: next,
      });
      onChange();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  /// What a plan narrowed this way would move, with the token that runs it.
  const plannedMove = async (scope: string, dir: string) => {
    const preview = await post<Preview>("/preview", {
      kind: "plan-apply",
      params: { roles: ["copy"], [scope]: dir },
    });
    if (!preview.items.length) return undefined;
    return {
      token: preview.token,
      files: preview.items.length,
      bytes: preview.items.reduce((n, i) => n + (i.size || 0), 0),
      scope,
    };
  };

  // The other half of naming a folder: one folder holds the originals, the
  // rest are copies of it — and a folder of copies is cleared in one plan
  // rather than group by group. Shown before it runs, because this is a
  // thousand files rather than one.
  const prepareFolderMove = async (dir: string) => {
    setBusy(true);
    setError("");
    try {
      const move = await plannedMove("folder", dir);
      setFolderNote({
        dir,
        // Nothing to move is an answer too, and a common one: a folder of
        // originals has no copies in it.
        text: move ? "" : ui.folderHasNoCopies,
        move,
      });
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const runFolderMove = async () => {
    if (!folderNote?.move) return;
    const { scope, token } = folderNote.move;
    setBusy(true);
    setError("");
    try {
      await post("/jobs", {
        kind: "plan-apply",
        params: { roles: ["copy"], [scope]: folderNote.dir },
        plan_token: token,
      });
      setFolderNote(null);
      setSelected(null);
      onChange();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const split = async (m: Member) => {
    setBusy(true);
    setError("");
    try {
      await post(`/families/${selected!.id}/split`, { file_id: m.file_id });
      onChange();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <div className="toolbar family-filters">
        <div className="search">
          <Icon name="search" size={17} />
          <input
            ref={searchRef}
            placeholder={ui.search}
            aria-label={ui.search}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
          <kbd>/</kbd>
        </div>
        <select
          aria-label={t("rol")}
          value={role}
          onChange={(e) => setRole(e.target.value)}
        >
          <option value="">{t("vse_roli")}</option>
          {[
            "original",
            "camera-jpg",
            "converted",
            "export",
            "copy",
            "resize",
            "unknown",
          ].map((r) => (
            <option key={r} value={r}>
              {r === "unknown" ? t("ne_opredeleno") : r.toUpperCase()}
            </option>
          ))}
        </select>
        <input
          className="short-input"
          aria-label={t("disk")}
          placeholder={t("disk")}
          value={disk}
          onChange={(e) => setDisk(e.target.value)}
        />
        <input
          className="short-input"
          type="number"
          min="0"
          aria-label={t("obyom_ot_mib")}
          placeholder={t("ot_mib")}
          value={min}
          onChange={(e) => setMin(e.target.value)}
        />
        <select
          aria-label={t("sortirovka")}
          value={sort}
          onChange={(e) => setSort(e.target.value)}
        >
          <option value="space">{t("po_vozvraschaemomu_obyomu")}</option>
          <option value="size">{t("po_obyomu_gruppy")}</option>
          <option value="count">{t("po_chislu_faylov")}</option>
          <option value="biggest">{t("po_samomu_bolshomu_faylu")}</option>
          <option value="date">{t("po_date")}</option>
          <option value="date-asc">{t("po_date_staryye")}</option>
          <option value="path">{t("po_papke")}</option>
        </select>
      </div>
      <div className="section-heading">
        <span className="muted">
          {number(total)} {t("semeystv")}
        </span>
        <label className="check">
          <input
            type="checkbox"
            checked={!all}
            onChange={(e) => setAll(!e.target.checked)}
          />
          {t("tolko_neskolko_faylov")}
        </label>
      </div>
      {error && <ErrorBox message={error} />}
      {moved && (
        <Notice>{t("perenesyono_v_karantin", moved.names.join(", "))}</Notice>
      )}
      <div className="family-workspace">
        <div className="family-list-pane">
          <div
            className="family-list"
            ref={listRef}
            onScroll={(e) => setScroll(e.currentTarget.scrollTop)}
            aria-label={t("spisok_semeystv")}
          >
            {r.error ? (
              <ErrorBox message={r.error} retry={r.reload} />
            ) : !total ? (
              r.loading ? (
                <Loading />
              ) : (
                <Empty
                  title={ui.noResults}
                  action={
                    <Button
                      onClick={() => {
                        setSearch("");
                        setRole("");
                        setDisk("");
                        setMin("");
                        setAll(true);
                      }}
                    >
                      {ui.reset}
                    </Button>
                  }
                >
                  {t("postroyte_semeystva_ili_izmenite_filtry")}
                </Empty>
              )
            ) : (
              <div style={{ height: total * rowHeight, position: "relative" }}>
                {Array.from(
                  {
                    length: Math.max(
                      0,
                      Math.min(
                        total,
                        Math.ceil((scroll + height) / rowHeight) + 3,
                      ) - first,
                    ),
                  },
                  (_, n) => {
                    const index = first + n,
                      f = cache.get(index);
                    return (
                      <div
                        key={index}
                        style={{
                          position: "absolute",
                          top: index * rowHeight,
                          left: 0,
                          right: 0,
                          height: rowHeight,
                        }}
                      >
                        {f ? (
                          <button
                            className={`family-list-item ${selected?.id === f.id ? "active" : ""}${
                              // Sorted by folder, the list walks the archive
                              // section by section, and a line where the
                              // folder changes is what makes that visible.
                              sort === "path" &&
                              cache.get(index - 1)?.members[0]?.dir !==
                                f.members[0]?.dir
                                ? " folder-start"
                                : ""
                            }`}
                            title={f.members[0]?.dir}
                            onClick={() => setSelected(f)}
                          >
                            <Thumb
                              thumb={f.members[0]?.thumb}
                              name={f.members[0]?.name || ""}
                            />
                            <span>
                              <strong>
                                {f.members[0]?.name || `#${f.id}`}
                              </strong>
                              <small>
                                {day(f.taken_at)} · {f.members.length}{" "}
                                {t("faylov")}
                              </small>
                              <span
                                className={
                                  f.removable_bytes ? "green" : "muted"
                                }
                              >
                                {f.removable_bytes
                                  ? `${bytes(f.removable_bytes)} ${t("v_kopiyah")}`
                                  : ui.noExactCopies}
                              </span>
                            </span>
                          </button>
                        ) : (
                          <div className="skeleton" />
                        )}
                      </div>
                    );
                  },
                )}
              </div>
            )}
          </div>
        </div>
        <section className="family-detail">
          {selected ? (
            <>
              <div className="family-detail-header">
                <div>
                  <div className="eyebrow">
                    {t("semeystvo")}
                    {selected.id}
                  </div>
                  <h2>{selected.members[0]?.name.replace(/\.[^.]+$/, "")}</h2>
                  <p className="muted">
                    {when(selected.taken_at)} ·{" "}
                    {selected.camera || ui.unknownCamera}
                  </p>
                </div>
                <div className="align-right">
                  <strong>{bytes(selected.total_size)}</strong>
                  <span className="muted">
                    {selected.members.length} {t("faylov")}
                  </span>
                </div>
              </div>
              <p className="muted">{ui.familyActionsHelp}</p>
              {/* Ten thousand groups is not a decision anybody makes in one
                  press. This moves the exact copies of *this* group, after
                  showing which files they are — the same plan as the one on
                  the plan screen, narrowed to what is on screen. */}
              {!!selected.removable_bytes && (
                <div className="group-apply">
                  <Button
                    icon="arrow"
                    kind="primary"
                    disabled={disabled || busy}
                    title={ui.groupApplyHelp}
                    onClick={() => moveGroup(selected)}
                  >
                    {ui.groupApply} · {bytes(selected.removable_bytes)}
                  </Button>
                  <span className="muted">{ui.groupApplyHelp}</span>
                </div>
              )}
              {/* The labels are the whole argument for what gets moved, so
                  they are explained where they are read rather than in
                  documentation nobody opens. */}
              <details className="role-legend">
                <summary>{ui.roleLegend}</summary>
                <dl>
                  {[
                    "original",
                    "camera-jpg",
                    "converted",
                    "export",
                    "resize",
                    "copy",
                    "unknown",
                  ].map((role) => (
                    <div key={role}>
                      <dt className={`role ${role}`}>
                        {role === "unknown" ? "?" : role.toUpperCase()}
                      </dt>
                      <dd>{roleHelp(role)}</dd>
                    </div>
                  ))}
                </dl>
              </details>
              <div className="section-heading">
                <span className="muted">{t("versii_snimka")}</span>
                <Button
                  disabled={selection.length !== 2}
                  onClick={() =>
                    setView({
                      images: selected.members.filter((m) =>
                        selection.includes(m.file_id),
                      ),
                      start: 0,
                    })
                  }
                >
                  {ui.compare}{" "}
                  {selection.length ? `(${selection.length}/2)` : ""}
                </Button>
              </div>
              <div className="member-tree">
                {selected.members.map((m) => (
                  <article
                    key={m.file_id}
                    tabIndex={0}
                    aria-label={m.name}
                    className={`member ${m.role === "copy" || m.role === "resize" ? "derived" : ""} ${m.is_keeper ? "kept" : ""} ${focused === m.file_id ? "focused" : ""}`}
                    onFocus={() => setFocused(m.file_id)}
                  >
                    <label className="compare-check">
                      <input
                        type="checkbox"
                        aria-label={t("sravnit", m.name, m.dir)}
                        checked={selection.includes(m.file_id)}
                        disabled={
                          !selection.includes(m.file_id) &&
                          selection.length >= 2
                        }
                        onChange={(e) =>
                          setSelection((s) =>
                            e.target.checked
                              ? [...s.slice(-1), m.file_id]
                              : s.filter((x) => x !== m.file_id),
                          )
                        }
                      />
                    </label>
                    <Thumb
                      thumb={m.thumb}
                      name={m.name}
                      onClick={() =>
                        setView({
                          images: selected.members,
                          start: selected.members.indexOf(m),
                        })
                      }
                    />
                    <div className="member-info">
                      <div className="inline">
                        <span
                          className={`role ${m.role}`}
                          title={roleHelp(m.role)}
                        >
                          {m.role === "unknown" ? "?" : m.role.toUpperCase()}
                        </span>
                        {m.is_keeper && (
                          <span className="keeper">★ {ui.keeper}</span>
                        )}
                      </div>
                      <strong>{m.name}</strong>
                      <div className="member-folder">
                        <code className="path" title={`${m.dir}/${m.name}`}>
                          {m.dir}
                        </code>
                        {/* Quiet, and under the path they act on: these are
                            about the folder, not about this file, and they
                            must not read as the main thing to press. */}
                        <div className="folder-actions">
                          <button
                            className="link"
                            disabled={disabled || busy}
                            title={ui.preferFolderHelp}
                            onClick={() => preferFolder(m.dir)}
                          >
                            {ui.preferFolder}
                          </button>
                          <span className="muted">·</span>
                          <button
                            className="link"
                            disabled={disabled || busy}
                            title={ui.moveFolderHelp}
                            onClick={() => prepareFolderMove(m.dir)}
                          >
                            {ui.moveFolder}
                          </button>
                        </div>
                        {folderNote?.dir === m.dir && (
                          <div className="folder-note">
                            {folderNote.text && <p>{folderNote.text}</p>}
                            {folderNote.move && (
                              <>
                                <p>
                                  {t(
                                    folderNote.move.scope === "keeper_folder"
                                      ? "dubli_etih_grupp"
                                      : "iz_papki_uedet",
                                    number(folderNote.move.files),
                                    bytes(folderNote.move.bytes),
                                    m.dir,
                                  )}
                                </p>
                                <div className="inline">
                                  <Button
                                    kind="primary"
                                    disabled={disabled || busy}
                                    onClick={runFolderMove}
                                  >
                                    {ui.move}
                                  </Button>
                                  <Button
                                    disabled={busy}
                                    onClick={() => setFolderNote(null)}
                                  >
                                    {ui.cancel}
                                  </Button>
                                </div>
                              </>
                            )}
                            {!folderNote.move && (
                              <Button
                                disabled={busy}
                                onClick={() => setFolderNote(null)}
                              >
                                {ui.close}
                              </Button>
                            )}
                          </div>
                        )}
                      </div>
                      <div className="muted">
                        {m.width} × {m.height} · {bytes(m.size)}
                      </div>
                      {m.evidence?.detail && <small>{m.evidence.detail}</small>}
                      {/* Marked a copy when the group was built, against a
                          different file than the one kept now. Saying so is
                          the difference between a button that does nothing
                          and a group the user can finish. */}
                      {m.role === "copy" && !m.is_keeper && !m.same_as_kept && (
                        <small className="warning-text">
                          {ui.notACopyOfKept}
                        </small>
                      )}
                      <details>
                        <summary>
                          {t("pochemu_eta_otsenka")}
                          {Math.round(m.quality)}
                        </summary>
                        <p>{m.breakdown}</p>
                      </details>
                      <details>
                        <summary>{t("podrobnosti_snimka")}</summary>
                        <FileDetails fileId={m.file_id} />
                      </details>
                      {!!m.catalogs?.length && (
                        <div className="lr-mark">
                          ▣ LR: {m.catalogs.join(", ")}{" "}
                          {m.rating ? "★".repeat(Math.min(m.rating, 5)) : ""}
                        </div>
                      )}
                      {m.sidecars?.map((path) => (
                        <div key={path} className="sidecar" title={path}>
                          └ sidecar · {basename(path)}
                        </div>
                      ))}
                      <div className="member-actions">
                        <Button
                          disabled={disabled || busy || m.is_keeper}
                          title={ui.setKeeperHelp}
                          onClick={() => keeper(m.file_id)}
                        >
                          {ui.setKeeper}
                        </Button>
                        <Button
                          disabled={
                            disabled || busy || selected.members.length < 2
                          }
                          title={ui.splitHelp}
                          onClick={() => split(m)}
                        >
                          {ui.split}
                        </Button>
                      </div>
                    </div>
                  </article>
                ))}
              </div>
            </>
          ) : (
            <Empty icon="layers" title={t("vyberite_semeystvo")}>
              {t("zdes_poyavyatsya_versii_snimka_i_svyazi_mezhdu_nimi")}
            </Empty>
          )}
        </section>
      </div>
      {view && (
        <ImageViewer
          images={view.images}
          start={view.start}
          onClose={() => setView(null)}
        />
      )}
    </>
  );
}
export function SeriesPage({
  revision,
  disabled,
  onChange,
}: {
  revision: number;
  disabled: boolean;
  onChange: () => void;
}) {
  const [offset, setOffset] = useState(0),
    [view, setView] = useState<{ images: ImageRef[]; start: number } | null>(
      null,
    ),
    [error, setError] = useState(""),
    [hideCopies, setHideCopies] = useState(false),
    [busy, setBusy] = useState(false);
  const r = useResource<{ total: number; series: Series[] }>(
    `/series?offset=${offset}&limit=20`,
    revision,
  );
  const act = async (path: string, body: unknown = {}) => {
    setBusy(true);
    setError("");
    try {
      await post(path, body);
      onChange();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <Notice>{ui.seriesNote}</Notice>
      <Notice>{ui.seriesPickHelp}</Notice>
      {error && <ErrorBox message={error} />}
      <Resource r={r}>
        {!r.data?.series.length ? (
          <Empty icon="series" title={t("serii_poka_ne_naydeny")}>
            {t("zapustite_sborku_na_ekrane_opis_i_indeks")}
          </Empty>
        ) : (
          r.data.series.map((s) => {
            // A burst of fifteen frames is often three photographs and their
            // copies. Numbering the groups says which frames are the same
            // file, and the strip can show one of each instead.
            const groups = new Map<number, number>();
            s.members.forEach((m) => {
              if (
                m.family_id !== null &&
                m.family_size > 1 &&
                !groups.has(m.family_id)
              )
                groups.set(m.family_id, groups.size + 1);
            });
            const position = new Map<number, number>();
            const copies = s.members.filter((m) => m.family_size > 1).length;
            const frames = hideCopies
              ? s.members.filter(
                  (m) => m.family_size <= 1 || m.is_family_keeper,
                )
              : s.members;
            return (
              <section className="panel series-panel" key={s.id}>
                <div className="section-heading">
                  <h3>
                    {s.label} · {when(s.started_at)}
                  </h3>
                  <span className="muted">
                    {s.camera || ui.unknownCamera} · {s.members.length}{" "}
                    {t("kadrov")}
                    {s.members.some((m) => m.is_rejected) &&
                      ` · ${t("otklonено_n", s.members.filter((m) => m.is_rejected).length)}`}
                  </span>
                </div>
                {copies > 0 && (
                  <div className="inline copies-line">
                    <span className="muted" title={ui.copiesInBurstHelp}>
                      {t("kopiy_v_serii", copies, groups.size)}
                    </span>
                    <label className="check">
                      <input
                        type="checkbox"
                        checked={hideCopies}
                        onChange={(e) => setHideCopies(e.target.checked)}
                      />
                      {ui.hideCopies}
                    </label>
                  </div>
                )}
                {!s.protected && (
                  <div className="inline">
                    <Button
                      disabled={disabled || busy}
                      title={ui.rejectRestHelp}
                      onClick={() => act(`/series/${s.id}/reject-rest`)}
                    >
                      {ui.rejectRest}
                    </Button>
                    <Button
                      disabled={
                        disabled ||
                        busy ||
                        !s.members.some((m) => m.is_rejected)
                      }
                      onClick={() => act(`/series/${s.id}/keep-all`)}
                    >
                      {ui.keepAll}
                    </Button>
                  </div>
                )}
                {s.protected && (
                  <Notice tone="warning">{ui.protectedSeries}</Notice>
                )}
                <div className="filmstrip">
                  {frames.map((m, i) => {
                    const group =
                      m.family_id !== null
                        ? groups.get(m.family_id)
                        : undefined;
                    const nth = group
                      ? (position.get(m.family_id!) || 0) + 1
                      : 0;
                    if (group) position.set(m.family_id!, nth);
                    return (
                      <article
                        className={`shot${m.is_best ? " best" : ""}${
                          m.is_rejected ? " rejected" : ""
                        }${group ? " copy" : ""}${
                          group && !m.is_family_keeper ? " spare" : ""
                        }`}
                        key={m.file_id}
                      >
                        {group && (
                          <span
                            className={`dupe-badge${m.is_family_keeper ? " kept" : ""}`}
                            title={ui.copiesInBurstHelp}
                          >
                            {m.is_family_keeper
                              ? t("gruppa_hranimyy", group, m.family_size)
                              : t("gruppa_kopiya", group, nth, m.family_size)}
                          </span>
                        )}
                        <Thumb
                          thumb={m.thumb}
                          name={m.name}
                          onClick={() =>
                            setView({
                              images: frames,
                              start: frames.indexOf(m),
                            })
                          }
                        />
                        <strong>
                          {i + 1}. {m.name}
                        </strong>
                        <span className="muted">
                          {t("mesto_po_kachestvu", m.rank + 1)} {t("rezkost")}
                          {m.sharpness?.toFixed(1) || "—"}
                        </span>
                        <p>{m.breakdown}</p>
                        <details>
                          <summary>{t("podrobnosti_snimka")}</summary>
                          <FileDetails fileId={m.file_id} />
                        </details>
                        <div className="shot-actions">
                          <Button
                            disabled={disabled || busy || m.is_best}
                            kind={m.is_best ? "selected" : ""}
                            onClick={() =>
                              act(`/series/${s.id}/best`, {
                                file_id: m.file_id,
                              })
                            }
                          >
                            {m.is_best ? `★ ${ui.best}` : ui.setBest}
                          </Button>
                          <Button
                            disabled={disabled || busy}
                            kind={m.is_rejected ? "danger-outline" : ""}
                            title={
                              m.is_rejected
                                ? ui.keepFrameHelp
                                : ui.rejectFrameHelp
                            }
                            onClick={() =>
                              act(`/files/${m.file_id}/reject`, {
                                rejected: !m.is_rejected,
                              })
                            }
                          >
                            {m.is_rejected ? ui.keepFrame : ui.rejectFrame}
                          </Button>
                        </div>
                      </article>
                    );
                  })}
                </div>
              </section>
            );
          })
        )}
        {(r.data?.total || 0) > 20 && (
          <div className="pagination">
            <Button
              disabled={!offset}
              onClick={() => setOffset(Math.max(0, offset - 20))}
            >
              {ui.back}
            </Button>
            <span>
              {offset + 1}–{Math.min(offset + 20, r.data!.total)} /{" "}
              {number(r.data!.total)}
            </span>
            <Button
              disabled={offset + 20 >= r.data!.total}
              onClick={() => setOffset(offset + 20)}
            >
              {ui.next}
            </Button>
          </div>
        )}
      </Resource>
      {view && (
        <ImageViewer
          images={view.images}
          start={view.start}
          onClose={() => setView(null)}
        />
      )}
    </>
  );
}
export function Categories({
  revision,
  disabled,
  onChange,
  start,
}: {
  revision: number;
  disabled: boolean;
  onChange: () => void;
  start: Start;
}) {
  const r = useResource<Category[]>("/categories", revision),
    [filter, setFilter] = useState(""),
    [search, setSearch] = useState(""),
    [view, setView] = useState<{ images: ImageRef[]; start: number } | null>(
      null,
    ),
    [error, setError] = useState("");
  const options = [
    ["photo", t("fotografiya")],
    ["document", t("dokumenty_i_skany")],
    ["screenshot", t("skrinshot")],
    ["blank", t("pustoy_kadr")],
    ["monochrome", t("monohrom")],
  ];
  const files = (r.data || [])
    .filter((g) => !filter || g.key === filter)
    .flatMap((g) => g.files.map((f) => ({ ...f, category: g.key })))
    .filter((f) =>
      `${f.dir}/${f.name}`.toLowerCase().includes(search.toLowerCase()),
    );
  const [columns, setColumns] = useState(window.innerWidth <= 800 ? 2 : 4);
  useEffect(() => {
    const update = () => setColumns(window.innerWidth <= 800 ? 2 : 4);
    window.addEventListener("resize", update);
    return () => window.removeEventListener("resize", update);
  }, []);
  const rows = Array.from(
    { length: Math.ceil(files.length / columns) },
    (_, i) => files.slice(i * columns, i * columns + columns),
  );
  // What the page is actually about: which kinds are in the archive and how
  // much of each. Without this the page is a flat wall of tiles that looks
  // the same whether it separated something out or nothing at all.
  const groups = (r.data || []).filter((g) => g.count > 0);
  const onlyPhotos = groups.length === 1 && groups[0].key === "photo";
  return (
    <>
      <div className="kind-summary">
        {groups.map((g) => (
          <button
            key={g.key}
            className={filter === g.key ? "current" : ""}
            onClick={() => setFilter(filter === g.key ? "" : g.key)}
          >
            <strong>{number(g.count)}</strong>
            <span>{g.label}</span>
            <small>{bytes(g.bytes)}</small>
          </button>
        ))}
      </div>
      {onlyPhotos && <Notice>{ui.onlyPhotos}</Notice>}
      <div className="inline">
        <Button
          disabled={disabled}
          onClick={async () => {
            setError("");
            try {
              await start("categories", {});
            } catch (e) {
              setError((e as Error).message);
            }
          }}
        >
          {ui.rebuildCategories}
        </Button>
        <span className="muted">{ui.rebuildCategoriesHelp}</span>
      </div>
      <div className="toolbar">
        <div className="search">
          <Icon name="search" size={16} />
          <input
            placeholder={ui.search}
            aria-label={ui.search}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
        </div>
        <select
          aria-label={t("vid_izobrazheniy")}
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        >
          <option value="">{t("vse_vidy")}</option>
          {r.data?.map((g) => (
            <option key={g.key} value={g.key}>
              {g.label} · {number(g.count)}
            </option>
          ))}
        </select>
      </div>
      {error && <ErrorBox message={error} />}
      <Resource r={r}>
        <VirtualList
          items={rows}
          resetKey={`${filter}|${search}`}
          rowHeight={330}
          height={650}
          render={(row) => (
            <div className="photo-grid">
              {row.map((f) => (
                <article className="photo-card" key={f.file_id}>
                  <Thumb
                    thumb={f.thumb}
                    name={f.name}
                    // Everything the page is showing, in the order it shows
                    // it: the arrows then walk the whole kind. Handing the
                    // viewer one row of the grid made four unrelated files
                    // look like a group, and every one of them opened the
                    // same four.
                    onClick={() =>
                      setView({ images: files, start: files.indexOf(f) })
                    }
                  />
                  <strong title={`${f.dir}/${f.name}`}>{f.name}</strong>
                  {/* Two files of the same name in two folders are two files,
                      and without this the grid shows what looks like the same
                      card twice. */}
                  <code className="path" title={f.dir}>
                    {f.dir}
                  </code>
                  <span className="muted">
                    {f.width} × {f.height} · {bytes(f.size)}
                  </span>
                  <select
                    disabled={disabled}
                    aria-label={t("vid", f.name)}
                    value={f.category}
                    onChange={async (e) => {
                      try {
                        await post(`/files/${f.file_id}/category`, {
                          category: e.target.value,
                        });
                        onChange();
                      } catch (e) {
                        setError((e as Error).message);
                      }
                    }}
                  >
                    {options.map(([key, label]) => (
                      <option key={key} value={key}>
                        {label}
                      </option>
                    ))}
                  </select>
                  <span
                    className={f.manual ? "green" : "muted"}
                    title={f.evidence}
                  >
                    {f.manual
                      ? `✓ ${ui.manual}`
                      : t("uverennost", Math.round(f.confidence * 100))}
                  </span>
                </article>
              ))}
            </div>
          )}
        />
      </Resource>
      {view && (
        <ImageViewer
          images={view.images}
          start={view.start}
          onClose={() => setView(null)}
        />
      )}
    </>
  );
}
