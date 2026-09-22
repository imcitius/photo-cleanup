import { useEffect, useState } from "react";
import { post } from "./api";
import {
  Button,
  Empty,
  ErrorBox,
  Icon,
  Loading,
  Modal,
  Notice,
  Thumb,
} from "./components";
import { Families as DetailedFamilies } from "./curation";
import { bytes, number, t, ui } from "./i18n";
import { ReviewComparison } from "./review-comparison";
import {
  openReviewedPlan,
  useReviewQueue,
  type Batch,
  type Decision,
} from "./use-review-queue";

export function ReviewShortcuts() {
  return (
    <dl className="keyboard-help">
      {[
        ["A", t("rq_add")],
        ["S", t("rq_keep")],
        ["D", t("rq_defer")],
        ["J / K", t("sleduyuschee_predyduschee_semeystvo")],
        ["Z", t("rq_zoom")],
        ["P", t("rq_open_plan")],
        ["⌘ / Ctrl Z", t("rq_undo")],
      ].map(([key, text]) => (
        <div key={key}>
          <dt>
            <kbd>{key}</kbd>
          </dt>
          <dd>{text}</dd>
        </div>
      ))}
    </dl>
  );
}
const decisionName = (state: string) =>
  state === "plan"
    ? t("rq_planned")
    : state === "keep"
      ? t("rq_kept")
      : state === "defer"
        ? t("rq_deferred")
        : t("rq_pending");
export function ReviewQueue(props: {
  revision: number;
  disabled: boolean;
  onChange: () => void;
}) {
  const [detailed, setDetailed] = useState(false);
  return detailed ? (
    <>
      <Button icon="arrow-left" onClick={() => setDetailed(false)}>
        {t("rq_back")}
      </Button>
      <DetailedFamilies {...props} />
    </>
  ) : (
    <Queue {...props} onDetailed={() => setDetailed(true)} />
  );
}
function Queue({
  revision,
  disabled,
  onChange,
  onDetailed,
}: {
  revision: number;
  disabled: boolean;
  onChange: () => void;
  onDetailed: () => void;
}) {
  const q = useReviewQueue(revision, disabled, onChange);
  const [zoom, setZoom] = useState(false),
    [help, setHelp] = useState(false),
    [batch, setBatch] = useState<Batch | null>(null),
    [batchBusy, setBatchBusy] = useState(false),
    [batchError, setBatchError] = useState("");
  useEffect(() => setZoom(false), [q.current?.id]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (
        e.repeat ||
        e.isComposing ||
        e.altKey ||
        document.querySelector("dialog[open]") ||
        (e.target instanceof HTMLElement &&
          e.target.closest('input,textarea,select,[contenteditable="true"]'))
      )
        return;
      if (e.ctrlKey || e.metaKey) {
        if (
          e.code === "KeyZ" &&
          !e.shiftKey &&
          q.history.length &&
          !q.blocked
        ) {
          e.preventDefault();
          void q.undo();
        }
        return;
      }
      const key = e.code || `Key${e.key.toUpperCase()}`;
      if (
        !["KeyA", "KeyS", "KeyD", "KeyJ", "KeyK", "KeyZ", "KeyP"].includes(key)
      )
        return;
      e.preventDefault();
      if (key === "KeyP") {
        openReviewedPlan();
        return;
      }
      if (q.blocked) return;
      if (key === "KeyZ") setZoom((v) => !v);
      else if (key === "KeyJ" || key === "KeyK")
        q.move(key === "KeyJ" ? 1 : -1);
      else
        void q.decide(
          (
            { KeyA: "plan", KeyS: "keep", KeyD: "defer" } as Record<
              string,
              Decision
            >
          )[key],
        );
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  });
  const previewBatch = async () => {
    setBatchBusy(true);
    setBatchError("");
    try {
      setBatch(await post<Batch>("/review/batch-preview"));
    } catch (e) {
      setBatchError((e as Error).message);
    } finally {
      setBatchBusy(false);
    }
  };
  const g = q.current;
  return (
    <div className="review-queue">
      <div className="queue-intro">
        <div>
          <h2>{t("rq_title")}</h2>
          <p className="muted">{t("rq_intro")}</p>
        </div>
        <Button
          disabled={!q.history.length || q.blocked}
          onClick={() => void q.undo()}
        >
          {t("rq_undo")} <kbd>⌘/Ctrl Z</kbd>
        </Button>
      </div>
      <div className="queue-toolbar">
        <label className="search-field">
          <Icon name="search" />
          <input
            type="search"
            aria-label={t("rq_search")}
            placeholder={t("rq_search")}
            value={q.search}
            onChange={(e) => q.filter(e.target.value, "search")}
            disabled={q.busy}
          />
        </label>
        <label className="inline">
          {t("rq_queue")}
          <select
            aria-label={t("rq_queue")}
            value={q.queue}
            disabled={q.busy}
            onChange={(e) => q.filter(e.target.value, "queue")}
          >
            <option value="pending">{t("rq_pending")}</option>
            <option value="defer">{t("rq_deferred")}</option>
            <option value="all">{t("rq_all_states")}</option>
            <option value="plan">{t("rq_planned")}</option>
            <option value="keep">{t("rq_kept")}</option>
          </select>
        </label>
        <Button onClick={() => setHelp(true)}>
          {ui.keyboard} <kbd>?</kbd>
        </Button>
      </div>
      <div className="queue-filter-row">
        <div className="segmented">
          {[
            ["all", t("rq_all")],
            ["exact", t("rq_exact")],
            ["versions", t("rq_versions")],
          ].map(([key, name]) => (
            <Button
              key={key}
              kind={q.kind === key ? "selected" : ""}
              aria-pressed={q.kind === key}
              disabled={q.busy}
              onClick={() => q.filter(key, "kind")}
            >
              {name}
            </Button>
          ))}
        </div>
        <div className="inline">
          <Button disabled={q.blocked || batchBusy} onClick={previewBatch}>
            {t("rq_batch")}
          </Button>
          <Button onClick={openReviewedPlan}>
            {t("rq_open_plan")} · {number(q.r.data?.counts.plan || 0)}{" "}
            <kbd>P</kbd>
          </Button>
        </div>
      </div>
      <div className="queue-progress">
        <span>
          {t(
            "rq_progress",
            number(q.r.data?.counts.pending || 0),
            number(q.r.data?.counts.defer || 0),
          )}
        </span>
        {g && (
          <label>
            {t("rq_position")}
            <input
              aria-label={t("rq_position")}
              type="number"
              min="1"
              max={q.total}
              value={q.cursor + 1}
              disabled={q.blocked}
              onChange={(e) => {
                if (e.target.value) q.jump(+e.target.value - 1);
              }}
            />
            / {number(q.total)}
          </label>
        )}
        <Button onClick={onDetailed} disabled={q.busy}>
          {t("rq_detailed")}
        </Button>
      </div>
      {(q.error || batchError) && (
        <ErrorBox message={q.error || batchError} retry={q.r.reload} />
      )}
      {q.r.error ? (
        <ErrorBox message={q.r.error} retry={q.r.reload} />
      ) : q.r.loading && !q.r.data ? (
        <Loading />
      ) : !g ? (
        <Empty
          title={t("rq_empty")}
          action={
            <Button
              onClick={() =>
                q.filter(q.queue === "defer" ? "pending" : "defer", "queue")
              }
            >
              {q.queue === "defer" ? t("rq_pending") : t("rq_deferred")}
            </Button>
          }
        >
          {t("rq_empty_hint")}
        </Empty>
      ) : (
        <>
          <div className="queue-strip" aria-label={t("rq_queue")}>
            {q.groups.slice(q.start, q.start + 3).map((group, i) => (
              <button
                className="queue-group"
                key={group.id}
                aria-pressed={group.id === g.id}
                disabled={q.blocked}
                onClick={() => q.select(q.start + i)}
              >
                <Thumb thumb={group.members[0]?.thumb} name="" />
                <span>
                  <strong>{group.members[0]?.name}</strong>
                  <small>
                    {group.members.length} {t("faylov")} ·{" "}
                    {group.exact ? t("rq_exact") : t("rq_versions")}
                  </small>
                  <span className="green">
                    {decisionName(group.review_state)}
                  </span>
                </span>
              </button>
            ))}
          </div>
          <div className="queue-desk" aria-busy={q.blocked}>
            <ReviewComparison
              key={g.id}
              group={g}
              zoom={zoom}
              setZoom={setZoom}
            />
            <aside className="queue-decision">
              <span className="eyebrow">{t("rq_evidence")}</span>
              <h3>{g.can_plan ? t("rq_verified") : t("rq_needs_review")}</h3>
              <p>{g.can_plan ? t("rq_exact_help") : t("rq_not_eligible")}</p>
              {g.review_reasons?.length > 0 && (
                <ul className="queue-reasons">
                  {g.review_reasons.map((reason, i) => (
                    <li key={i}>{reason}</li>
                  ))}
                </ul>
              )}
              {g.members.some((m) => m.is_rejected) && (
                <Notice>{t("rq_manual_choices")}</Notice>
              )}
              <div className="queue-decision-buttons">
                <Button
                  kind="primary"
                  disabled={
                    q.blocked || !g.can_plan || g.review_state === "plan"
                  }
                  onClick={() => void q.decide("plan")}
                >
                  {t("rq_add")} <kbd>A</kbd>
                </Button>
                <Button
                  disabled={q.blocked || g.review_state === "keep"}
                  onClick={() => void q.decide("keep")}
                >
                  {t("rq_keep")} <kbd>S</kbd>
                </Button>
                <Button
                  disabled={q.blocked || g.review_state === "defer"}
                  onClick={() => void q.decide("defer")}
                >
                  {t("rq_defer")} <kbd>D</kbd>
                </Button>
              </div>
              <small className="muted">{t("rq_after")}</small>
              <div className="queue-navigation">
                <Button
                  disabled={q.blocked || q.cursor === 0}
                  onClick={() => q.move(-1)}
                >
                  ← <kbd>K</kbd>
                </Button>
                <Button
                  disabled={q.blocked || q.cursor >= q.total - 1}
                  onClick={() => q.move(1)}
                >
                  {ui.nextFrame} <kbd>J</kbd> →
                </Button>
              </div>
            </aside>
          </div>
        </>
      )}
      <p className="queue-note" role="status">
        {q.note || t("rq_no_move")}
      </p>
      {help && (
        <Modal title={ui.keyboard} onClose={() => setHelp(false)}>
          <ReviewShortcuts />
          <p>{t("rq_shortcuts_hint")}</p>
        </Modal>
      )}
      {batch && (
        <Modal title={t("rq_batch")} onClose={() => !q.busy && setBatch(null)}>
          <p>{t("rq_batch_scope")}</p>
          <div className="queue-batch-total">
            <strong>
              {number(batch.groups)} {t("semeystv")}
            </strong>
            <span>
              {number(batch.files)} {t("faylov")} · {bytes(batch.bytes)}
            </span>
          </div>
          <Notice>{t("rq_batch_help")}</Notice>
          <div className="modal-actions">
            <Button disabled={q.busy} onClick={() => setBatch(null)}>
              {ui.cancel}
            </Button>
            <Button
              kind="primary"
              disabled={q.blocked || !batch.groups}
              onClick={async () => {
                await q.applyBatch(batch);
                setBatch(null);
              }}
            >
              {t("rq_batch_confirm")}
            </Button>
          </div>
        </Modal>
      )}
    </div>
  );
}
