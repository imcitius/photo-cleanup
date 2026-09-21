import { t } from "./i18n";
import { useEffect, useState } from "react";
import { api, post, useDebounce, useResource } from "./api";
import {
  Button,
  Empty,
  ErrorBox,
  Icon,
  Loading,
  Modal,
  Notice,
  Resource,
  Thumb,
  Totals,
  VirtualList,
} from "./components";
import {
  basename,
  bytes,
  duration,
  jobName,
  jobTone,
  number,
  ui,
  stateName,
  when,
} from "./i18n";
import type { Job, Journal, PlanItem, Preview, RecentFile, Run } from "./types";
export type Start = (
  kind: string,
  params?: Record<string, unknown>,
  plan?: Preview,
  confirmation?: string,
) => Promise<void>;
export function useJobs() {
  const [jobs, setJobs] = useState<Job[]>([]),
    [error, setError] = useState(""),
    [revision, setRevision] = useState(0);
  const load = () =>
    api<Job[]>("/jobs")
      .then((data) => {
        setJobs((old) => {
          if (
            old.length &&
            old.some(
              (j) =>
                ["running", "queued"].includes(j.state) &&
                !data.some(
                  (n) =>
                    n.id === j.id && ["running", "queued"].includes(n.state),
                ),
            )
          )
            setRevision((n) => n + 1);
          return data;
        });
        setError("");
      })
      .catch((e) => setError(e.message));
  useEffect(() => {
    load();
    const timer = setInterval(load, 5000);
    const visible = () => {
      if (!document.hidden) load();
    };
    document.addEventListener("visibilitychange", visible);
    return () => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", visible);
    };
  }, []);
  const active = jobs.find((j) => ["running", "queued"].includes(j.state));
  useEffect(() => {
    if (!active) return;
    let stopped = false,
      source: EventSource | undefined,
      timer: ReturnType<typeof setTimeout>,
      backoff = 1000;
    const connect = () => {
      if (stopped) return;
      source = new EventSource(`/api/jobs/${active.id}/events`);
      source.onmessage = (e) => {
        const job = JSON.parse(e.data) as Job;
        backoff = 1000;
        setError("");
        setJobs((old) => old.map((j) => (j.id === job.id ? job : j)));
        if (!["running", "queued"].includes(job.state)) {
          source?.close();
          setRevision((n) => n + 1);
          load();
        }
      };
      source.onerror = () => {
        source?.close();
        setError(t("svyaz_s_zadachey_prervana_perepodklyuchaemsya"));
        api<Job>(`/jobs/${active.id}`)
          .then((job) =>
            setJobs((old) => old.map((j) => (j.id === job.id ? job : j))),
          )
          .catch(() => {});
        timer = setTimeout(connect, backoff);
        backoff = Math.min(30000, backoff * 2);
      };
    };
    connect();
    return () => {
      stopped = true;
      source?.close();
      clearTimeout(timer);
    };
  }, [active?.id]);
  const start: Start = async (kind, params = {}, plan, confirmation) => {
    await post("/jobs", {
      kind,
      params,
      plan_token: plan?.token,
      confirmation,
    });
    await load();
    setRevision((n) => n + 1);
  };
  return {
    jobs,
    active,
    error,
    revision,
    start,
    refresh: () => {
      setRevision((n) => n + 1);
      load();
    },
  };
}
/**
 * What the index has produced so far, refreshed while the job runs.
 *
 * A progress bar tells you how far along a pass is; it does not tell you
 * whether the pass is doing anything sensible. Indexing commits every couple
 * of seconds, so the last two dozen rows are always close to the file being
 * read right now — and a wall of photographs from the right folders is the
 * fastest way to know the run is worth leaving alone.
 */
function LiveIndex({ job }: { job: Job }) {
  const [files, setFiles] = useState<RecentFile[]>([]);
  useEffect(() => {
    let stopped = false;
    const load = () =>
      api<RecentFile[]>("/recent?limit=24")
        .then((rows) => {
          if (!stopped) setFiles(rows);
        })
        .catch(() => {});
    load();
    const timer = setInterval(load, 3000);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  }, [job.id]);
  if (!files.length) return null;
  return (
    <div className="live-index">
      <div className="section-heading">
        <strong>{ui.liveTitle}</strong>
        <span className="muted">{ui.liveHint}</span>
      </div>
      <div className="live-strip">
        {files.map((f) => (
          <figure
            key={f.id}
            className={f.skipped_reason ? "skipped" : ""}
            title={
              f.skipped_reason ? `${f.path} — ${f.skipped_reason}` : f.path
            }
          >
            <Thumb thumb={f.thumb_key} name={f.name} />
            <figcaption>{f.name}</figcaption>
          </figure>
        ))}
      </div>
    </div>
  );
}

export function JobProgress({
  id,
  job,
  onCancel,
}: {
  id?: string;
  job: Job;
  onCancel: () => Promise<void>;
}) {
  const [stopping, setStopping] = useState(false),
    [error, setError] = useState("");
  const p = job.progress;
  const pct = p.total ? Math.min(100, ((p.done || 0) / p.total) * 100) : null;
  return (
    <section
      id={id}
      className="job-progress"
      aria-label={t("progress_zadachi")}
    >
      <div className="section-heading">
        <div className="inline">
          <span className="spinner" />
          <strong>{jobName(job.kind)}</strong>
          {!!p.steps && (
            <span className="badge">{t("shag_iz", p.step, p.steps)}</span>
          )}
          <span className="muted">{p.phase || stateName(job.state)}</span>
        </div>
        <Button
          disabled={stopping}
          onClick={() => {
            setStopping(true);
            onCancel().catch((e) => {
              setStopping(false);
              setError(e.message);
            });
          }}
        >
          {stopping ? ui.stopping : ui.stop}
        </Button>
      </div>
      <div
        className="progress-track"
        role="progressbar"
        aria-label={jobName(job.kind)}
        aria-valuenow={pct ?? undefined}
        aria-valuemin={0}
        aria-valuemax={100}
      >
        <span
          className={pct === null ? "indeterminate" : ""}
          style={{ width: pct === null ? "35%" : `${pct}%` }}
        />
      </div>
      <div className="section-heading">
        <span>
          {number(p.done || 0)}
          {p.total ? ` / ${number(p.total)}` : ""} · {bytes(p.bytes_done || 0)}
          {p.bytes_total ? ` / ${bytes(p.bytes_total)}` : ""}
        </span>
        <span className="muted">
          {p.eta_secs != null
            ? t("ostalos_okolo_min", Math.max(1, Math.ceil(p.eta_secs / 60)))
            : t("otsenivaem_vremya")}
        </span>
      </div>
      <code className="current-path">{p.current}</code>
      {(p.per_disk || []).length > 0 && (
        <div className="disk-progress">
          {p.per_disk!.map((d) => (
            <div key={d.disk}>
              <Icon name="server" size={15} />
              <span>{d.disk}</span>
              <progress
                aria-label={t("progress", d.disk)}
                value={d.done}
                max={d.total || 1}
              />
              <span>
                {number(d.done)} / {number(d.total)}
              </span>
            </div>
          ))}
        </div>
      )}
      <p className="muted">
        {stopping ? ui.stopNote : ui.jobNote} {p.note}
      </p>
      {["index", "scan", "all"].includes(job.kind) && (
        <p className="muted">{ui.liveResume}</p>
      )}
      {["index", "all"].includes(job.kind) && <LiveIndex job={job} />}
      {error && <ErrorBox message={error} />}
    </section>
  );
}
/**
 * What a finished job left behind.
 *
 * "Completed with refusals" is not a sentence anyone can act on, and on a
 * first index every single one of them is a video file the tool was never
 * going to read. So the refusals are grouped by reason and counted, the
 * headline says plainly what happened, and the warning colour is kept for
 * jobs that actually failed.
 */
export function JobResult({
  job,
  onDismiss,
}: {
  job: Job;
  onDismiss: () => void;
}) {
  const refusals = job.progress.refusals || [];
  const byReason = Array.from(Map.groupBy(refusals, ([, why]) => why)).sort(
    (a, b) => b[1].length - a[1].length,
  );
  const broken = job.state === "failed" || job.state === "interrupted";
  return (
    <Notice tone={broken ? "warning" : "info"}>
      <div className="section-heading">
        <strong>
          {t("zadacha")}
          {job.id} · {jobName(job.kind)}:{" "}
          {job.error ||
            (refusals.length
              ? t("propuscheno_faylov", number(refusals.length))
              : stateName(job.state))}
        </strong>
        <Button
          icon="close"
          aria-label={t("skryt_rezultat_zadachi")}
          onClick={onDismiss}
        />
      </div>
      {!!refusals.length && (
        <>
          <p className="muted">{ui.skippedNote}</p>
          {byReason.map(([why, rows]) => (
            <details key={why}>
              <summary>
                {why} · {number(rows.length)}
              </summary>
              <div className="result-refusals">
                {rows.slice(0, 200).map(([path], i) => (
                  <p key={i}>
                    <code>{path}</code>
                  </p>
                ))}
                {rows.length > 200 && (
                  <p className="muted">
                    {t("i_eschyo", number(rows.length - 200))}
                  </p>
                )}
              </div>
            </details>
          ))}
        </>
      )}
      <a href="#journal">{t("proverit_zhurnal")}</a>
    </Notice>
  );
}

export function Refusals({ items }: { items: Preview["refusals"] }) {
  const groups = Map.groupBy(items, (r) => r.why);
  return (
    <div className="refusals">
      {Array.from(groups).map(([why, rows]) => (
        <details key={why}>
          <summary>
            <span className="badge warning">! {rows.length}</span>
            {why}
          </summary>
          <VirtualList
            items={rows}
            rowHeight={56}
            height={280}
            render={(r) => <code className="path refusal-path">{r.path}</code>}
          />
        </details>
      ))}
    </div>
  );
}
export function PlanRows({
  items,
  onDate,
}: {
  items: PlanItem[];
  onDate?: (items: PlanItem[]) => void;
}) {
  const [companions, setCompanions] = useState<PlanItem | null>(null);
  return (
    <>
      <VirtualList
        items={items}
        rowHeight={130}
        render={(item) => (
          <div className="plan-row">
            <div className="plan-source">
              <Thumb thumb={item.thumb} name={basename(item.path)} />
              <div>
                <strong>{basename(item.path)}</strong>
                <code className="path" title={item.path}>
                  {item.path}
                </code>
                <span
                  className={`badge ${item.uncertain ? "warning" : ""}${
                    item.manual ? " manual" : ""
                  }`}
                >
                  {item.manual
                    ? ui.markedByHand
                    : item.source ||
                      item.role?.toUpperCase() ||
                      bytes(item.size)}
                </span>
                {!!item.companions?.length && (
                  <Button onClick={() => setCompanions(item)}>
                    {t("sputniki")}
                    {item.companions.length}
                  </Button>
                )}
                {item.renamed_from && (
                  <span className="warning-text">
                    {t("imya_zanyato")}
                    {item.renamed_from}
                  </span>
                )}
              </div>
            </div>
            <Icon name="arrow" size={16} />
            <div className="plan-destination">
              <code className="path" title={item.dst}>
                {item.dst}
              </code>
              {item.keeper_path && (
                <div className="keeper-mini">
                  <Thumb
                    thumb={item.keeper_thumb}
                    name={basename(item.keeper_path)}
                  />
                  <div>
                    <span className="green">✓ {ui.willStay}</span>
                    <code className="path">{item.keeper_path}</code>
                  </div>
                </div>
              )}
              {item.reason && <small>{item.reason}</small>}
              {onDate && item.file_id && (
                <Button onClick={() => onDate([item])}>
                  {t("ispravit_datu")}
                </Button>
              )}
            </div>
            <span className="num">{bytes(item.size)}</span>
          </div>
        )}
      />
      {companions && (
        <Modal title={t("sputniki_snimka")} onClose={() => setCompanions(null)}>
          <p>
            {t(
              "eti_fayly_peremeschayutsya_vmeste_so_snimkom_i_vklyucheny_v_obsch",
            )}
          </p>
          {companions.companions?.map((c) => (
            <div className="companion-plan" key={c.path}>
              <code>{c.path}</code>
              <Icon name="arrow" size={16} />
              <code>{c.dst}</code>
              <span>{bytes(c.size)}</span>
            </div>
          ))}
        </Modal>
      )}
    </>
  );
}
export function Review({
  kind,
  params,
  disabled,
  start,
  organize = false,
  onDate,
  refresh = 0,
}: {
  kind: string;
  params: Record<string, unknown>;
  disabled: boolean;
  start: Start;
  organize?: boolean;
  onDate?: (items: PlanItem[]) => void;
  refresh?: number;
}) {
  const [plan, setPlan] = useState<Preview | null>(null),
    [loading, setLoading] = useState(true),
    [error, setError] = useState(""),
    [nonce, setNonce] = useState(0),
    [confirm, setConfirm] = useState(false),
    [word, setWord] = useState(""),
    [accepted, setAccepted] = useState(false),
    [sending, setSending] = useState(false),
    [uncertain, setUncertain] = useState(false);
  const key = JSON.stringify(params),
    debounced = useDebounce(key);
  const stale = key !== debounced;
  useEffect(() => {
    setPlan(null);
    setError("");
    setLoading(true);
    setConfirm(false);
    const controller = new AbortController();
    api<Preview>("/preview", {
      method: "POST",
      body: JSON.stringify({ kind, params: JSON.parse(debounced) }),
      signal: controller.signal,
    })
      .then(setPlan)
      .catch((e) => {
        if (e.name !== "AbortError") setError(e.message);
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
  }, [kind, debounced, nonce, refresh]);
  const purge = kind === "derived-purge" || kind === "quarantine-purge";
  const apply = async () => {
    if (!plan) return;
    setSending(true);
    try {
      await start(kind, params, plan, word);
      setConfirm(false);
      setNonce((n) => n + 1);
    } catch (e) {
      setError((e as Error).message);
      setConfirm(false);
    } finally {
      setSending(false);
    }
  };
  const filtered = plan?.items.filter((i) => !uncertain || i.uncertain) || [];
  return (
    <div className="review">
      {loading || stale ? (
        <Loading />
      ) : error ? (
        <>
          <ErrorBox message={error} retry={() => setNonce((n) => n + 1)} />
          {organize && <a href="#plan">{t("pereyti_k_planu_dublikatov")}</a>}
        </>
      ) : (
        plan && (
          <>
            <section className="review-summary">
              <div>
                <div className="eyebrow">{ui.preview}</div>
                <Totals files={plan.total_files} size={plan.total_bytes} />
                <p className="muted">
                  {purge
                    ? t("budet_udaleno_okonchatelno")
                    : kind.includes("undo")
                      ? t("vernutsya_po_ishodnym_putyam")
                      : t("budet_pereneseno_posle_podtverzhdeniya")}{" "}
                  {t("otkazov")}
                  {number(plan.refusals.length)}
                </p>
                {/* These did not come from the role checkboxes above, so the
                    numbers would otherwise look as if they had. */}
                {plan.items.some((i) => i.manual) && (
                  <p className="muted">
                    {t(
                      "otmecheno_vruchnuyu_n",
                      number(plan.items.filter((i) => i.manual).length),
                    )}
                  </p>
                )}
              </div>
              <Button
                kind={purge ? "danger" : "primary"}
                disabled={disabled || !plan.items.length}
                onClick={() => {
                  setWord("");
                  setAccepted(false);
                  setConfirm(true);
                }}
                icon={purge ? "trash" : "arrow"}
              >
                {purge
                  ? ui.purge
                  : kind.includes("undo")
                    ? ui.undo
                    : organize
                      ? t("razlozhit_po_datam")
                      : ui.move}
              </Button>
            </section>
            {plan.items.length > 0 && (
              <p className="muted">{ui.planColumnsHelp}</p>
            )}
            {plan.items.length === 0 ? (
              <Empty title={ui.noCandidates}>
                {t("izmenite_parametry_ili_proverte_prichiny_otkazov_nizhe")}
              </Empty>
            ) : organize ? (
              <>
                <div className="section-heading">
                  <h3>
                    {t("novoe_derevo")}
                    {
                      new Set(plan.items.map((i) => `${i.year}/${i.event}`))
                        .size
                    }{" "}
                    {t("sobytiy")}
                  </h3>
                  <label className="check">
                    <input
                      type="checkbox"
                      checked={uncertain}
                      onChange={(e) => setUncertain(e.target.checked)}
                    />
                    {t("tolko_nenadyozhnye_daty")}
                  </label>
                </div>
                <OrganizeTree items={filtered} onDate={onDate} />
              </>
            ) : (
              <PlanRows items={plan.items} />
            )}
            <div className="section-heading">
              <h3>{ui.refusals}</h3>
              <span className="muted">{number(plan.refusals.length)}</span>
            </div>
            {plan.refusals.length ? (
              <Refusals items={plan.refusals} />
            ) : (
              <p className="muted">
                {t("otkazov_net_proverki_povtoryatsya_pered_kazhdym_perenosom")}
              </p>
            )}
          </>
        )
      )}
      {confirm && plan && (
        <Modal
          title={
            purge
              ? ui.purge
              : kind.includes("undo")
                ? t("vosstanovit_fayly")
                : t("vypolnit_pokazannyy_plan")
          }
          onClose={() => !sending && setConfirm(false)}
        >
          <Totals files={plan.total_files} size={plan.total_bytes} />
          <p>
            {t("pokazannyy_spisok")}
            {plan.items.length} {t("operatsiy_otkazov")}
            {plan.refusals.length}.
          </p>
          <p className="muted">
            {t(
              "puti_naznacheniya_ukazany_v_predprosmotre_esli_sostav_plana_izmen",
            )}
          </p>
          {purge ? (
            <>
              <Notice tone="error">{ui.purgeWarning}</Notice>
              <label className="check">
                <input
                  type="checkbox"
                  checked={accepted}
                  onChange={(e) => setAccepted(e.target.checked)}
                />
                {t(
                  "ya_proveril_rezervnuyu_kopiyu_i_ponimayu_chto_otkata_ne_budet",
                )}
              </label>
              <label className="field">
                {t("vvedite")}
                {ui.purgeWord}
                <input
                  autoComplete="off"
                  value={word}
                  onChange={(e) => setWord(e.target.value)}
                />
              </label>
            </>
          ) : (
            <Notice>
              {t(
                "operatsiya_obratima_cherez_zhurnal_fayly_perenosyatsya_v_predelah",
              )}
            </Notice>
          )}
          <div className="modal-actions">
            <Button onClick={() => setConfirm(false)} disabled={sending}>
              {ui.cancel}
            </Button>
            <Button
              kind={purge ? "danger" : "primary"}
              disabled={
                sending ||
                disabled ||
                (purge && (!accepted || word !== ui.purgeWord))
              }
              onClick={apply}
            >
              {sending ? t("zapuskaem") : purge ? ui.purge : ui.apply}
            </Button>
          </div>
        </Modal>
      )}
    </div>
  );
}
export function JournalPage({
  revision,
  start,
  disabled,
}: {
  revision: number;
  start: Start;
  disabled: boolean;
}) {
  const [run, setRun] = useState(""),
    [op, setOp] = useState(""),
    [status, setStatus] = useState(""),
    [review, setReview] = useState<{
      kind: string;
      params: Record<string, unknown>;
    } | null>(null);
  const r = useResource<Journal[]>(
      `/journal?${new URLSearchParams({ run, op, status })}`,
      revision,
    ),
    history = useResource<Job[]>("/jobs", revision),
    runs = useResource<Run[]>("/runs", revision);
  return (
    <>
      <div className="toolbar">
        <label>
          {t("progon")}
          <select value={run} onChange={(e) => setRun(e.target.value)}>
            <option value="">{t("vse_progony")}</option>
            {runs.data?.map((r) => (
              <option key={r.id} value={r.id}>
                №{r.id} · {when(r.started_at)}
              </option>
            ))}
          </select>
        </label>
        <label>
          {t("operatsiya")}
          <select value={op} onChange={(e) => setOp(e.target.value)}>
            <option value="">{t("vse_operatsii")}</option>
            <option value="quarantine">{t("prevyu_v_karantin")}</option>
            <option value="quarantine-file">{t("fayl_v_karantin")}</option>
            <option value="organize">{t("raskladka")}</option>
          </select>
        </label>
        <label>
          {t("status")}
          <select value={status} onChange={(e) => setStatus(e.target.value)}>
            <option value="">{t("vse_statusy")}</option>
            {["pending", "done", "failed", "undone", "purged"].map((s) => (
              <option key={s} value={s}>
                {stateName(s)}
              </option>
            ))}
          </select>
        </label>
      </div>
      <Resource r={r}>
        {r.data?.some((x) => x.status === "pending") && (
          <Notice tone="warning">{ui.pendingNote}</Notice>
        )}
        {/* An empty journal after a long evening of indexing looks like a bug.
            It is not: nothing was moved, so there is nothing to record. */}
        {!r.data?.length && (
          <Notice tone="info">{ui.journalEmptyExplained}</Notice>
        )}
        <VirtualList
          items={r.data || []}
          resetKey={`${run}|${op}|${status}`}
          rowHeight={126}
          render={(j) => (
            <div className="journal-row">
              <div>
                <span
                  className={`badge ${j.status === "pending" ? "warning" : ""}`}
                >
                  {stateName(j.status)}
                </span>
                <strong>
                  #{j.id} ·{" "}
                  {j.op === "organize"
                    ? t("raskladka")
                    : j.op === "quarantine"
                      ? t("prevyu_v_karantin")
                      : t("fayl_v_karantin")}
                </strong>
                <span className="muted">
                  {when(j.applied_at)} {t("progon_2")}
                  {j.run_id}
                </span>
                <code className="path">{j.src}</code>
                <code className="path">→ {j.dst}</code>
                {j.note && <small>{j.note}</small>}
              </div>
              <div className="align-right">
                <span>{bytes(j.size)}</span>
                {j.status === "done" && (
                  <Button
                    onClick={() =>
                      setReview({
                        kind: "journal-undo",
                        params: { journal_id: j.id },
                      })
                    }
                  >
                    {ui.undo}
                  </Button>
                )}
              </div>
            </div>
          )}
        />
      </Resource>
      <h3>{ui.doneSoFar}</h3>
      <Resource r={history}>
        <VirtualList
          items={history.data || []}
          rowHeight={76}
          height={320}
          render={(j) => (
            <div className="run-row">
              <span className={`badge ${jobTone(j.state)}`}>
                {stateName(j.state)}
              </span>
              <strong>
                #{j.id} {jobName(j.kind)}
              </strong>
              <span>{when(j.started_at)}</span>
              <span>
                {j.finished_at
                  ? `${ui.duration}: ${duration(j.finished_at - j.started_at)}`
                  : ""}
              </span>
              <span className="muted">
                {j.error ||
                  (j.progress?.total
                    ? `${number(j.progress.done || 0)} / ${number(j.progress.total)}`
                    : "")}
              </span>
            </div>
          )}
        />
      </Resource>
      <h3>{t("progony")}</h3>
      <Resource r={runs}>
        <VirtualList
          items={runs.data || []}
          rowHeight={76}
          height={300}
          render={(r) => (
            <div className="run-row">
              <strong>#{r.id}</strong>
              <span>{when(r.started_at)}</span>
              <span>
                {r.operations} {t("operatsiy")}
                {bytes(r.bytes)}
              </span>
              {!!r.undoable && (
                <Button
                  onClick={() =>
                    setReview({
                      kind: "organize-undo",
                      params: { run_id: r.id },
                    })
                  }
                >
                  {ui.undoRun}
                </Button>
              )}
            </div>
          )}
        />
      </Resource>
      {review && (
        <Modal title={ui.preview} wide onClose={() => setReview(null)}>
          <Review {...review} start={start} disabled={disabled} />
        </Modal>
      )}
    </>
  );
}

function OrganizeTree({
  items,
  onDate,
}: {
  items: PlanItem[];
  onDate?: (items: PlanItem[]) => void;
}) {
  const [selected, setSelected] = useState("");
  const years = Array.from(Map.groupBy(items, (i) => i.year)).sort(
    ([a], [b]) => (a || 0) - (b || 0),
  );
  return (
    <>
      {years.map(([year, yearItems], index) => {
        const events = Array.from(Map.groupBy(yearItems, (i) => i.event));
        const chosen = events.find(
          ([event]) => selected === `${year}/${event}`,
        );
        return (
          <details className="year" key={year} open={index === 0}>
            <summary>
              <Icon name="folder" />
              {year}{" "}
              <span className="muted">
                {yearItems.length} {t("faylov")}
              </span>
            </summary>
            <VirtualList
              items={events}
              rowHeight={86}
              height={344}
              render={([event, eventItems]) => (
                <button
                  className="event-select"
                  onClick={() => setSelected(`${year}/${event}`)}
                >
                  <Thumb
                    thumb={eventItems[0].event_cover || eventItems[0].thumb}
                    name={event || ""}
                  />
                  <strong>{event}</strong>
                  <span>
                    {eventItems.length} {t("faylov")}
                  </span>
                  <span className="num">
                    {bytes(eventItems.reduce((s, i) => s + i.size, 0))}
                  </span>
                  <Icon name="arrow" size={16} />
                </button>
              )}
            />
            {chosen && (
              <div className="event-files">
                <div className="section-heading">
                  <h3>{chosen[0]}</h3>
                  <Button onClick={() => onDate?.(chosen[1])}>
                    {t("zadat_datu_vsemu_sobytiyu")}
                  </Button>
                </div>
                <PlanRows items={chosen[1]} onDate={onDate} />
              </div>
            )}
          </details>
        );
      })}
    </>
  );
}
