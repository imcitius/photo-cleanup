import { t } from "./i18n";
import { useEffect, useState } from "react";
import { api, post, useResource } from "./api";
import {
  Button,
  Empty,
  ErrorBox,
  FolderPicker,
  Icon,
  Modal,
  Notice,
  Resource,
  Thumb,
  Totals,
  VirtualList,
} from "./components";
import {
  bytes,
  jobName,
  looksLikeArray,
  number,
  ui,
  stateName,
  when,
} from "./i18n";
import { LANGUAGES, setLanguage } from "./i18n";
import { ImageViewer, type ImageRef } from "./curation";
import { Review, type Start } from "./workflow";
import type {
  Bundle,
  Catalog,
  Job,
  Journal,
  Page,
  PlanItem,
  QuarantineItem,
  Settings,
  Status,
} from "./types";

export function Overview({
  status,
  jobs,
  revision,
  navigate,
}: {
  status: Status | null;
  jobs: Job[];
  revision: number;
  navigate: (p: Page) => void;
}) {
  const journal = useResource<Journal[]>("/journal?status=pending", revision),
    catalogs = useResource<Catalog[]>("/catalogs", revision),
    settings = useResource<Settings>("/settings", revision),
    bundles = useResource<Bundle[]>("/derived", revision);
  const onArray = looksLikeArray(settings.data?.roots);
  // A combined job stands in for every stage it contains, or the pipeline
  // reports "not started" for work that plainly ran.
  const covers: Record<string, string[]> = {
    "build-all": ["families", "series", "categories"],
    all: ["scan", "index", "families", "series", "categories"],
  };
  const latest = (kind: string) =>
    jobs.find((j) => j.kind === kind || covers[j.kind]?.includes(kind));
  const indexed = latest("index");
  const stages: [string, string, Page, boolean][] = [
    ["scan", t("opis"), "setup", !!bundles.data?.length],
    ["index", t("indeks"), "setup", !!status?.images],
    ["families", t("semeystva"), "families", !!status?.families],
    ["series", t("serii"), "series", !!status?.series],
    ["categories", t("vidy"), "categories", !!status?.categorised],
    ["plan-apply", t("plan"), "plan", false],
    ["organize-apply", t("raskladka"), "organize", false],
  ];
  const stageState = (key: string, exists: boolean) => {
    const j = latest(key);
    if (j?.state === "running" || j?.state === "queued")
      return t("vypolnyaetsya");
    if (j?.state === "done") {
      if (
        ["families", "series", "categories"].includes(key) &&
        indexed &&
        indexed.id > j.id
      )
        return t("ustarelo");
      return t("gotovo");
    }
    return exists ? t("est_dannye") : t("ne_nachato");
  };
  const active = jobs.find((j) => ["running", "queued"].includes(j.state));
  const interrupted = jobs.filter((j) => j.state === "interrupted"),
    locked = catalogs.data?.filter((c) => c.is_locked) || [];
  const copies = status?.roles.find((r) => r.role === "copy")?.bytes || 0,
    resizes = status?.roles.find((r) => r.role === "resize")?.bytes || 0,
    derived = status?.derived_removable_bytes || 0;
  const first = !status?.images;
  const next = first
    ? {
        text: ui.firstTitle,
        desc: ui.firstText,
        button: ui.chooseRoots,
        page: "setup" as Page,
      }
    : !status.families
      ? {
          text: t("naydyom_versii_odnogo_snimka"),
          desc: t(
            "indeks_gotov_postroyte_semeystva_serii_i_vidy_chtoby_nachat_razbo",
          ),
          button: t("pereyti_k_sborke"),
          page: "setup" as Page,
        }
      : derived > 0
        ? {
            text: t("nachnite_s_prevyu_i_keshey"),
            desc: t(
              "proverte_proizvodnye_dannye_lightroom_ih_mozhno_vosstanovit_iz_or",
            ),
            button: t("proverit_prevyu"),
            page: "derived" as Page,
          }
        : {
            text: t("proverte_plan_tochnyh_kopiy"),
            desc: t(
              "dlya_kazhdogo_kandidata_pokazhem_kakoy_fayl_ostayotsya_i_pochemu_",
            ),
            button: t("otkryt_plan"),
            page: "plan" as Page,
          };
  return (
    <>
      {(interrupted.length > 0 || !!journal.data?.length) && (
        <Notice tone="warning">
          <strong>{t("est_nezavershyonnaya_rabota")}</strong>
          <p>
            {interrupted.map((j) => `№${j.id} ${jobName(j.kind)}`).join(", ")}{" "}
            {t("zapisey_dlya_proverki")}
            {journal.data?.length || 0}
          </p>
          <a href="#journal">{t("proverit_zhurnal")}</a>
        </Notice>
      )}
      {locked.length > 0 && (
        <Notice tone="warning">
          {t("otkryt_lightroom")}
          {locked.map((c) => c.name).join(", ")}
          {t("ego_prevyu_zaschischeny")}
        </Notice>
      )}
      <div className="parity-line">
        <Icon name="shield" size={16} />
        <strong>{onArray ? ui.parity : ui.backup}</strong>
        <span>{onArray ? ui.parityDetail : ui.backupDetail}</span>
      </div>
      <section className="next-step">
        <div>
          <div className="eyebrow">
            <span className="status-dot" />
            {active ? ui.nowRunning : ui.nextStep}
          </div>
          {/* While something is running, "what next" is the wrong question:
              the answer is "wait, and here is what for". */}
          {active ? (
            <>
              <h2>{jobName(active.kind)}</h2>
              <p>
                {active.progress.phase || stateName(active.state)}
                {!!active.progress.steps &&
                  ` · ${t("shag_iz", active.progress.step, active.progress.steps)}`}
                {!!active.progress.total &&
                  ` · ${number(active.progress.done || 0)} / ${number(active.progress.total)}`}
              </p>
              <p className="muted">{ui.liveResume}</p>
            </>
          ) : (
            <>
              <h2>{next.text}</h2>
              <p>{next.desc}</p>
              <Button
                kind="primary"
                icon="arrow"
                onClick={() => navigate(next.page)}
              >
                {next.button}
              </Button>
            </>
          )}
        </div>
        <div className="archive-illustration" aria-hidden="true">
          <div className="illustration-card back">
            <Icon name="image" size={34} />
          </div>
          <div className="illustration-card front">
            <Icon name="layers" size={38} />
            <div />
            <div />
            <span>
              <Icon name="check" size={15} />
            </span>
          </div>
          <div className="illustration-note">{t("arhiv_na_meste")}</div>
        </div>
      </section>
      <section className="panel pipeline-panel">
        <div className="section-heading">
          <div>
            <h3>{ui.pipeline}</h3>
            <p className="muted">{ui.pipelineText}</p>
          </div>
          <span className="small-label">{t("text_7_stadiy")}</span>
        </div>
        <div className="pipeline">
          {stages.map(([kind, label, page, exists], i) => {
            const state = stageState(kind, exists);
            return (
              <button
                key={kind}
                onClick={() => navigate(page)}
                className={
                  state === t("gotovo") || state === t("est_dannye")
                    ? "complete"
                    : state === t("vypolnyaetsya")
                      ? "current"
                      : ""
                }
              >
                <span className="stage-node">
                  {state === t("gotovo") || state === t("est_dannye") ? (
                    <Icon name="check" size={18} />
                  ) : (
                    i + 1
                  )}
                </span>
                <strong>{label}</strong>
                <small>{state}</small>
              </button>
            );
          })}
        </div>
      </section>
      <div className="overview-grid">
        <section className="panel reclaim">
          <div className="section-heading">
            <h3>{ui.recoverable}</h3>
            <span className="badge">{t("predvaritelnaya_otsenka")}</span>
          </div>
          <div className="big-number">
            {bytes(copies + derived)}
            <span>{t("prevyu_i_tochnye_kopii")}</span>
          </div>
          <div className="space-bar">
            <span
              style={{
                flex: derived || 1,
                background: !derived && !copies ? "var(--surface)" : undefined,
              }}
            />
            <span
              style={{
                flex: copies || 0.05,
                background: !derived && !copies ? "var(--surface)" : undefined,
              }}
            />
          </div>
          <button className="space-row" onClick={() => navigate("derived")}>
            <span>
              <i />
              {ui.previews}
            </span>
            <strong>{bytes(derived)}</strong>
            <Icon name="arrow" size={16} />
          </button>
          <button className="space-row" onClick={() => navigate("plan")}>
            <span>
              <i className="copy-dot" />
              {ui.copy}
            </span>
            <strong>{bytes(copies)}</strong>
            <Icon name="arrow" size={16} />
          </button>
          <button className="space-row" onClick={() => navigate("plan")}>
            <span>
              <i className="resize-dot" />
              {ui.resize} <small>{t("po_vyboru")}</small>
            </span>
            <strong>{bytes(resizes)}</strong>
            <Icon name="arrow" size={16} />
          </button>
          <p className="muted">
            {ui.spaceNote} {t("zaschischyonnye_fayly_isklyuchayutsya_v_plane")}
          </p>
        </section>
        <section className="panel archive-summary">
          <div className="section-heading">
            <h3>{t("arhiv_v_tsifrah")}</h3>
            <Icon name="server" size={19} />
          </div>
          <dl>
            <div>
              <dt>{t("izobrazheniy_v_indekse")}</dt>
              <dd>{number(status?.images || 0)}</dd>
            </div>
            <div>
              <dt>{t("semeystv_snimkov")}</dt>
              <dd>{number(status?.families || 0)}</dd>
            </div>
            <div>
              <dt>{t("s_neskolkimi_versiyami")}</dt>
              <dd>{number(status?.families_multi || 0)}</dd>
            </div>
            <div>
              <dt>{t("uzhe_v_karantine")}</dt>
              <dd>{bytes(status?.quarantined_bytes || 0)}</dd>
            </div>
          </dl>
          <Button icon="archive" onClick={() => navigate("quarantine")}>
            {t("otkryt_karantin")}
          </Button>
          {!!status?.skipped && (
            <p className="muted">
              {t("propuscheno_pri_indeksatsii")}
              {number(status.skipped)}
            </p>
          )}
        </section>
      </div>
      <section className="panel">
        <div className="section-heading">
          <h3>{t("poslednie_zadachi")}</h3>
          <a href="#journal">{t("zhurnal_operatsiy")}</a>
        </div>
        {jobs.length ? (
          <div>
            {jobs.slice(0, 5).map((j) => (
              <div className="activity-row" key={j.id}>
                <span
                  className={`activity-icon ${j.state === "done" ? "green" : ""}`}
                >
                  <Icon
                    name={
                      j.state === "done"
                        ? "check"
                        : j.state === "failed"
                          ? "alert"
                          : "clock"
                    }
                    size={17}
                  />
                </span>
                <div>
                  <strong>{jobName(j.kind)}</strong>
                  <span className="muted">
                    {j.error || j.progress.phase || t("zadacha_2", j.id)}
                  </span>
                </div>
                <span className="badge">{stateName(j.state)}</span>
                <time>{when(j.started_at)}</time>
              </div>
            ))}
          </div>
        ) : (
          <div className="quiet-empty">
            <Icon name="clock" />
            {t("zdes_poyavitsya_istoriya_vashey_raboty_s_arhivom")}
          </div>
        )}
      </section>
    </>
  );
}
export function Setup({
  settings,
  start,
  disabled,
  onSettings,
}: {
  settings: Settings;
  start: Start;
  disabled: boolean;
  onSettings: () => void;
}) {
  const [roots, setRoots] = useState(settings.roots || []),
    [folder, setFolder] = useState(false),
    [input, setInput] = useState(""),
    [min, setMin] = useState(settings.min_size),
    [readers, setReaders] = useState(2),
    [reindex, setReindex] = useState(false),
    [phash, setPhash] = useState(settings.phash_max),
    [ssim, setSsim] = useState(settings.ssim_min),
    [gap, setGap] = useState(settings.series_gap_secs),
    [error, setError] = useState(""),
    [busy, setBusy] = useState(false);
  const suggestions = (settings.suggested_roots || []).filter(
    (s) => !roots.includes(s),
  );
  const add = (s: string) => {
    if (s && !roots.includes(s)) setRoots([...roots, s]);
    setInput("");
  };
  const run = async (kind: string) => {
    setError("");
    setBusy(true);
    try {
      await api("/settings", {
        method: "PUT",
        body: JSON.stringify({ roots }),
      });
      await start(kind, {
        roots,
        min_size: min,
        readers_per_disk: readers,
        reindex,
        phash_max: phash,
        ssim_min: ssim,
        gap_secs: gap,
      });
      onSettings();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      {error && <ErrorBox message={error} />}
      <section className="panel setup-section">
        <span className="step-number">1</span>
        <div>
          <h2>{t("gde_hranyatsya_fotografii")}</h2>
          <p className="muted">{ui.rootsHint}</p>
          <p className="muted">{ui.rootsAreServerPaths}</p>
          {looksLikeArray(roots) && (
            <p className="muted">{ui.rootsHintArray}</p>
          )}
          <div className="root-list">
            {roots.map((root) => (
              <div key={root}>
                <Icon name="folder" />
                <code>{root}</code>
                <Button
                  icon="close"
                  aria-label={t("ubrat", root)}
                  onClick={() => setRoots(roots.filter((r) => r !== root))}
                />
              </div>
            ))}
          </div>
          <form
            className="inline"
            onSubmit={(e) => {
              e.preventDefault();
              add(input.trim());
            }}
          >
            <input
              aria-label={t("koren_arhiva")}
              placeholder={t("primer_puti_k_arhivu")}
              value={input}
              onChange={(e) => setInput(e.target.value)}
            />
            <Button type="submit" disabled={!input.trim()} icon="plus">
              {t("dobavit")}
            </Button>
            <Button type="button" icon="folder" onClick={() => setFolder(true)}>
              {t("obzor_papok")}
            </Button>
          </form>
          {/* What the server can actually see. In a container this is the
              only place the operator learns the paths inside it. */}
          {!roots.length && !!suggestions.length && (
            <div className="suggested-roots">
              <span className="muted">{ui.suggestedRoots}</span>
              {suggestions.map((s) => (
                <Button key={s} icon="plus" onClick={() => add(s)}>
                  {s}
                </Button>
              ))}
            </div>
          )}
        </div>
      </section>
      <section className="panel setup-section highlight">
        <Icon name="arrow" size={22} />
        <div>
          <h2>{ui.sdelatVsyo}</h2>
          <p className="muted">{ui.sdelatVsyoOpisanie}</p>
          <Button
            kind="primary"
            disabled={disabled || busy || !roots.length}
            onClick={() => run("all")}
          >
            {ui.sdelatVsyo}
          </Button>
        </div>
      </section>
      <p className="muted section-divider">{t("ili_po_stadiyam")}</p>
      <section className="panel setup-section">
        <span className="step-number">2</span>
        <div>
          <h2>{t("sostavte_opis")}</h2>
          <p className="muted">
            {t(
              "naydyom_katalogi_lightroom_prevyu_i_keshi_fotografii_ostanutsya_n",
            )}
          </p>
          <Button
            kind="primary"
            disabled={disabled || busy || !roots.length}
            onClick={() => run("scan")}
          >
            {t("nachat_opis")}
          </Button>
        </div>
      </section>
      <section className="panel setup-section">
        <span className="step-number">3</span>
        <div>
          <h2>{t("prochitayte_izobrazheniya")}</h2>
          <p className="muted">
            {t(
              "indeksatsiya_bolshogo_arhiva_mozhet_zanyat_neskolko_chasov_uzhe_p",
            )}
          </p>
          <div className="form-grid">
            <label className="field">
              {t("minimalnyy_razmer_bayt")}
              <input
                type="number"
                min="0"
                value={min}
                onChange={(e) => setMin(+e.target.value)}
              />
            </label>
            <label className="field">
              {t("chitateley_na_disk")}
              <input
                type="number"
                min="1"
                max="8"
                value={readers}
                onChange={(e) => setReaders(+e.target.value)}
              />
            </label>
          </div>
          <label className="check">
            <input
              type="checkbox"
              checked={reindex}
              onChange={(e) => setReindex(e.target.checked)}
            />
            {t("perechitat_uzhe_aktualnye_fayly")}
          </label>
          <Button
            kind="primary"
            disabled={disabled || busy || !roots.length}
            onClick={() => run("index")}
          >
            {t("nachat_indeksatsiyu")}
          </Button>
        </div>
      </section>
      <section className="panel setup-section">
        <span className="step-number">4</span>
        <div>
          <h2>{t("postroyte_kartu_arhiva")}</h2>
          <p className="muted">
            {t(
              "semeystva_serii_vidy_tsepochka_vypolnitsya_na_servere_dazhe_esli_",
            )}
          </p>
          <details>
            <summary>{t("porogi_analiza")}</summary>
            <div className="form-grid">
              <label className="field">
                {t("rasstoyanie_phash")}
                <input
                  type="number"
                  min="0"
                  max="64"
                  value={phash}
                  onChange={(e) => setPhash(+e.target.value)}
                />
              </label>
              <label className="field">
                {t("minimalnyy_ssim")}
                <input
                  type="number"
                  min="0"
                  max="1"
                  step="0.01"
                  value={ssim}
                  onChange={(e) => setSsim(+e.target.value)}
                />
              </label>
              <label className="field">
                {t("razryv_serii_sekund")}
                <input
                  type="number"
                  min="1"
                  value={gap}
                  onChange={(e) => setGap(+e.target.value)}
                />
              </label>
            </div>
          </details>
          <div className="inline">
            <Button
              kind="primary"
              disabled={disabled || busy}
              onClick={async () => {
                try {
                  await api("/settings", {
                    method: "PUT",
                    body: JSON.stringify({
                      phash_max: phash,
                      ssim_min: ssim,
                      series_gap_secs: gap,
                    }),
                  });
                  await run("build-all");
                } catch (e) {
                  setError((e as Error).message);
                }
              }}
            >
              {t("postroit_vsyo")}
            </Button>
            {["families", "series", "categories"].map((k) => (
              <Button
                key={k}
                disabled={disabled || busy}
                onClick={() => run(k)}
              >
                {jobName(k)}
              </Button>
            ))}
          </div>
        </div>
      </section>
      {folder && (
        <FolderPicker
          onClose={() => setFolder(false)}
          onChoose={add}
          initial={roots[0] || "/"}
        />
      )}
    </>
  );
}
export function Policy({
  disabled,
  start,
  revision,
}: {
  disabled: boolean;
  start: Start;
  revision: number;
}) {
  const [roles, setRoles] = useState(["copy"]),
    [resize, setResize] = useState(2),
    [allow, setAllow] = useState(false);
  return (
    <>
      <Notice>{ui.quarantineExplained}</Notice>
      <section className="panel">
        <h3>{t("kakie_versii_perenosit")}</h3>
        <p className="muted">
          {t(
            "original_vsegda_ostayotsya_v_arhive_po_umolchaniyu_vybirayutsya_t",
          )}
        </p>
        <div className="role-options">
          {[
            ["copy", t("tochnaya_kopiya")],
            ["resize", t("umenshennaya_versiya")],
            ["export", t("eksport")],
            ["converted", t("konvertatsiya")],
            ["camera-jpg", t("jpeg_kamery")],
            ["unknown", t("ne_opredeleno_2")],
          ].map(([role, label]) => (
            <label key={role} className={roles.includes(role) ? "checked" : ""}>
              <input
                type="checkbox"
                checked={roles.includes(role)}
                onChange={(e) =>
                  setRoles(
                    e.target.checked
                      ? [...roles, role]
                      : roles.filter((r) => r !== role),
                  )
                }
              />
              <span>
                <strong>{role === "unknown" ? "?" : role.toUpperCase()}</strong>
                <small>{label}</small>
              </span>
            </label>
          ))}
        </div>
        {roles.includes("resize") && (
          <label className="field">
            {t("resize_menshe_megapikseley")}
            <input
              type="number"
              min="0"
              step="0.1"
              value={resize}
              onChange={(e) => setResize(+e.target.value)}
            />
          </label>
        )}
        <label className="check">
          <input
            type="checkbox"
            checked={!allow}
            onChange={(e) => setAllow(!e.target.checked)}
          />
          {t("zaschischat_fayly_iz_katalogov_lightroom")}
        </label>
        {allow && <Notice tone="warning">{ui.lrWarning}</Notice>}
      </section>
      <Review
        kind="plan-apply"
        params={{
          roles,
          resize_below: Math.round(resize * 1e6),
          allow_lightroom: allow,
        }}
        start={start}
        disabled={disabled}
        refresh={revision}
      />
    </>
  );
}
export function Derived({
  disabled,
  start,
  revision,
}: {
  disabled: boolean;
  start: Start;
  revision: number;
}) {
  const r = useResource<Bundle[]>("/derived", revision),
    catalogs = useResource<Catalog[]>("/catalogs", revision),
    [kinds, setKinds] = useState(["lr-previews", "lr-helper", "system-junk"]),
    [min, setMin] = useState(0),
    [review, setReview] = useState(false);
  const groups = Map.groupBy(
    (r.data || []).filter((b) => b.state === "present"),
    (b) => b.kind,
  );
  return (
    <>
      <Notice>
        {t("prevyu_mozhno_peresozdat_esli_ishodniki_dostupny_smart_previews_p")}
      </Notice>
      <Resource r={r}>
        {!groups.size ? (
          <Empty
            title={t("proizvodnye_dannye_poka_ne_naydeny")}
            action={<a href="#setup">{t("nachat_opis_2")}</a>}
          />
        ) : (
          Array.from(groups).map(([kind, rows]) => (
            <section className="panel derived-group" key={kind}>
              <div className="section-heading">
                <label className="check">
                  <input
                    type="checkbox"
                    disabled={!rows.some((b) => b.regenerable)}
                    checked={kinds.includes(kind)}
                    onChange={(e) => {
                      setKinds(
                        e.target.checked
                          ? [...kinds, kind]
                          : kinds.filter((k) => k !== kind),
                      );
                      setReview(false);
                    }}
                  />
                  <div>
                    <h3>{rows[0].kind_label}</h3>
                    <span className="muted">
                      {number(rows.reduce((n, b) => n + b.file_count, 0))}{" "}
                      {t("faylov_2")}
                      {rows.length} {t("obektov")}
                    </span>
                  </div>
                </label>
                <strong>{bytes(rows.reduce((n, b) => n + b.size, 0))}</strong>
              </div>
              <VirtualList
                items={rows}
                rowHeight={95}
                height={285}
                render={(b) => (
                  <div className="bundle-row">
                    <Icon name={b.removable ? "folder" : "shield"} size={19} />
                    <div>
                      <code className="path">{b.path}</code>
                      <span className={b.removable ? "muted" : "warning-text"}>
                        {!b.regenerable
                          ? t("udalenie_zaprescheno_vidom_dannyh")
                          : b.blocked
                            ? t("zablokirovano", b.blocked)
                            : b.hint || t("mozhno_peresozdat")}
                      </span>
                    </div>
                    <span className="num">{bytes(b.size)}</span>
                  </div>
                )}
              />
            </section>
          ))
        )}
      </Resource>
      {!!groups.size && (
        <div className="toolbar">
          <label>
            {t("minimalnyy_razmer_mib")}
            <input
              type="number"
              min="0"
              value={min}
              onChange={(e) => {
                setMin(+e.target.value);
                setReview(false);
              }}
            />
          </label>
          <Button
            kind="primary"
            disabled={!kinds.length}
            onClick={() => setReview(true)}
          >
            {ui.preview}
          </Button>
        </div>
      )}
      {review && (
        <Review
          kind="derived-clean"
          params={{ kinds, min_size: Math.round(min * 1024 * 1024) }}
          disabled={disabled}
          start={start}
          refresh={revision}
        />
      )}
      <section className="panel">
        <h3>{t("katalogi_lightroom")}</h3>
        <Resource r={catalogs}>
          <VirtualList
            items={catalogs.data || []}
            rowHeight={100}
            height={300}
            render={(c) => (
              <div className="bundle-row">
                <Icon name="layers" />
                <div>
                  <strong>{c.name}</strong>
                  <code className="path">{c.path}</code>
                  <span className="muted">
                    {c.image_count ?? "—"} {t("izobrazheniy")}
                    {c.read_error && `· ${c.read_error}`}
                  </span>
                </div>
                <span className={`badge ${c.is_locked ? "warning" : ""}`}>
                  {c.is_locked
                    ? t("otkryt_3")
                    : c.is_backup
                      ? t("rezervnyy")
                      : t("zakryt")}
                </span>
              </div>
            )}
          />
        </Resource>
      </section>
    </>
  );
}
export function Quarantine({
  disabled,
  start,
  revision,
}: {
  disabled: boolean;
  start: Start;
  revision: number;
}) {
  const r = useResource<QuarantineItem[]>("/quarantine", revision),
    [days, setDays] = useState(7),
    [purge, setPurge] = useState(false),
    [view, setView] = useState<{ images: ImageRef[]; start: number } | null>(
      null,
    ),
    [undo, setUndo] = useState<number | null>(null);
  const items = r.data || [];
  // Only the photographs can be shown; a bundle of Lightroom previews has no
  // single frame to open.
  const viewable: ImageRef[] = items
    .filter((j) => j.file_id !== null)
    .map((j) => ({ file_id: j.file_id!, name: j.name, thumb: j.thumb }));
  return (
    <>
      <section className="panel">
        <div className="section-heading">
          <div>
            <h3>{t("hranyatsya_v_karantine")}</h3>
            <Totals
              files={items.reduce((s, j) => s + j.file_count, 0)}
              size={items.reduce((s, j) => s + j.size, 0)}
            />
          </div>
          <span className="badge">{t("mozhno_vernut")}</span>
        </div>
        <p className="muted">
          {t(
            "karantin_nahoditsya_na_tom_zhe_diske_perenos_syuda_eschyo_ne_osvo",
          )}
        </p>
        <Resource r={r}>
          <VirtualList
            items={items}
            rowHeight={124}
            render={(j) => (
              <div className="journal-row quarantine-row">
                <Thumb
                  thumb={j.thumb}
                  name={j.name}
                  onClick={
                    j.file_id === null
                      ? undefined
                      : () =>
                          setView({
                            images: viewable,
                            start: viewable.findIndex(
                              (v) => v.file_id === j.file_id,
                            ),
                          })
                  }
                />
                <div>
                  <div className="inline">
                    <strong>{j.name}</strong>
                    <span className="badge">{j.kind}</span>
                    <span className="muted">{when(j.applied_at)}</span>
                  </div>
                  <code className="path" title={j.dst || ""}>
                    {j.dst}
                  </code>
                  <small className="muted">
                    {t("ishodnyy_put")}
                    {j.src}
                  </small>
                </div>
                <span>{bytes(j.size)}</span>
                <Button onClick={() => setUndo(j.journal_id)}>{ui.undo}</Button>
              </div>
            )}
          />
        </Resource>
      </section>
      <section className="panel danger-zone">
        <div className="section-heading">
          <h3>{t("okonchatelnoe_udalenie")}</h3>
          <Icon name="trash" />
        </div>
        <p>{ui.purgeWarning}</p>
        <div className="toolbar">
          <label>
            {t("hranyatsya_ne_menee_dney")}
            <input
              type="number"
              min="0"
              max="3650"
              value={days}
              onChange={(e) => {
                setDays(+e.target.value);
                setPurge(false);
              }}
            />
          </label>
          <Button
            kind="danger-outline"
            onClick={() => setPurge(true)}
            disabled={!items.length}
          >
            {t("proverit_pered_udaleniem")}
          </Button>
        </div>
      </section>
      {purge && (
        <Review
          kind="derived-purge"
          params={{ older_than_secs: Math.round(days * 86400) }}
          start={start}
          disabled={disabled}
          refresh={revision}
        />
      )}{" "}
      {undo !== null && (
        <Modal
          title={t("plan_vosstanovleniya")}
          wide
          onClose={() => setUndo(null)}
        >
          <Review
            kind="journal-undo"
            params={{ journal_id: undo }}
            start={start}
            disabled={disabled}
          />
        </Modal>
      )}
      {view && (
        <ImageViewer
          images={view.images}
          start={Math.max(0, view.start)}
          onClose={() => setView(null)}
        />
      )}
    </>
  );
}
export function Organize({
  settings,
  disabled,
  start,
  revision,
}: {
  settings: Settings;
  disabled: boolean;
  start: Start;
  revision: number;
}) {
  const [root, setRoot] = useState(""),
    [picker, setPicker] = useState(false),
    [gap, setGap] = useState(settings.event_gap_secs / 3600),
    [skip, setSkip] = useState(false),
    [allow, setAllow] = useState(false),
    [duplicates, setDuplicates] = useState(false),
    [edit, setEdit] = useState<PlanItem[] | null>(null),
    [date, setDate] = useState(""),
    [error, setError] = useState(""),
    [nonce, setNonce] = useState(0);
  return (
    <>
      <section className="panel">
        <label className="field">
          {t("koren_novogo_dereva")}
          <div className="inline">
            <input
              placeholder={t("primer_puti_k_arhivu")}
              value={root}
              onChange={(e) => setRoot(e.target.value)}
            />
            <Button icon="folder" onClick={() => setPicker(true)}>
              {t("vybrat")}
            </Button>
          </div>
        </label>
        <p className="muted">
          {looksLikeArray(settings.roots) ? ui.rootsHintArray : ui.rootsHint}{" "}
          {t("fayly_s_drugogo_diska_budut_perechisleny_v_otkazah")}
        </p>
        <label className="field">
          {t("razryv_mezhdu_syomkami")}
          {gap} {t("ch")}
          <input
            type="range"
            min="0.25"
            max="72"
            step="0.25"
            value={gap}
            onChange={(e) => setGap(+e.target.value)}
          />
        </label>
        <div className="inline">
          <label className="check">
            <input
              type="checkbox"
              checked={skip}
              onChange={(e) => setSkip(e.target.checked)}
            />
            {t("propuskat_nenadyozhnye_daty")}
          </label>
          <label className="check">
            <input
              type="checkbox"
              checked={!allow}
              onChange={(e) => setAllow(!e.target.checked)}
            />
            {t("zaschischat_fayly_lightroom")}
          </label>
        </div>
        {allow && <Notice tone="warning">{ui.lrWarning}</Notice>}
        <details>
          <summary>{t("dopolnitelnoe_razreshenie")}</summary>
          <label className="check">
            <input
              type="checkbox"
              checked={duplicates}
              onChange={(e) => setDuplicates(e.target.checked)}
            />
            {t("razreshit_raskladku_nerazobrannyh_dublikatov")}
          </label>
          {duplicates && (
            <Notice tone="warning">
              {t(
                "kopii_tozhe_pereedut_v_novoe_derevo_rekomenduem_snachala_proverit",
              )}
              <a href="#plan">{t("plan_2")}</a>.
            </Notice>
          )}
        </details>
      </section>
      {root.trim() ? (
        <Review
          kind="organize-apply"
          params={{
            root: root.trim(),
            gap_secs: Math.round(gap * 3600),
            allow_lightroom: allow,
            skip_uncertain: skip,
            allow_duplicates: duplicates,
          }}
          organize
          onDate={(items) => {
            setEdit(items);
            setDate("");
            setError("");
          }}
          disabled={disabled}
          start={start}
          refresh={revision + nonce}
        />
      ) : (
        <Empty title={t("vyberite_papku_novogo_arhiva")}>
          {t("poyavitsya_derevo_sobytiy_i_tochnyy_put_kazhdogo_fayla")}
        </Empty>
      )}
      {picker && (
        <FolderPicker
          initial={root || settings.roots?.[0] || "/"}
          onClose={() => setPicker(false)}
          onChoose={setRoot}
        />
      )}{" "}
      {edit && (
        <Modal
          title={t("ispravit_datu_faylov", edit.length)}
          onClose={() => setEdit(null)}
        >
          <p>
            {ui.manualHint}{" "}
            {t(
              "data_zapisyvaetsya_kak_ukazannoe_vremya_syomki_bez_sdviga_chasovo",
            )}
          </p>
          <label className="field">
            {t("data_i_vremya")}
            <input
              type="datetime-local"
              value={date}
              onChange={(e) => setDate(e.target.value)}
            />
          </label>
          {error && <ErrorBox message={error} />}
          <div className="modal-actions">
            <Button
              disabled={!date || disabled}
              kind="primary"
              onClick={async () => {
                try {
                  await post(`/files/${edit[0].file_id}/date`, {
                    taken_at: Math.floor(new Date(`${date}Z`).getTime() / 1000),
                    file_ids: edit.map((i) => i.file_id),
                  });
                  setEdit(null);
                  setNonce((n) => n + 1);
                } catch (e) {
                  setError((e as Error).message);
                }
              }}
            >
              {ui.save}
            </Button>
          </div>
        </Modal>
      )}
    </>
  );
}
export function SettingsPage({
  settings,
  onSaved,
  disabled,
}: {
  settings: Settings;
  onSaved: () => void;
  disabled: boolean;
}) {
  const [value, setValue] = useState(settings),
    [error, setError] = useState(""),
    [saved, setSaved] = useState(false);
  useEffect(() => setValue(settings), [settings]);
  const field = (key: keyof Settings, label: string, step = 1) => (
    <label className="field">
      {label}
      <input
        type="number"
        step={step}
        value={value[key] as number}
        onChange={(e) => setValue({ ...value, [key]: +e.target.value })}
      />
    </label>
  );
  return (
    <form
      onSubmit={async (e) => {
        e.preventDefault();
        setError("");
        setSaved(false);
        try {
          await api("/settings", {
            method: "PUT",
            body: JSON.stringify(value),
          });
          setLanguage(value.language);
          localStorage.setItem("pc-theme", value.theme);
          localStorage.setItem("pc-density", value.density);
          document.documentElement.dataset.theme = value.theme;
          document.documentElement.dataset.density = value.density;
          onSaved();
          setSaved(true);
        } catch (e) {
          setError((e as Error).message);
        }
      }}
    >
      <section className="panel">
        <h3>{t("puti_na_servere")}</h3>
        <label className="field">
          {t("baza_dannyh")}
          <input readOnly value={settings.db_path} />
        </label>
        <label className="field">
          {t("kesh_prevyu")}
          <input readOnly value={settings.thumbs_path} />
        </label>
        <p className="muted">{t("baza_i_kesh_zadany_pri_zapuske_servera")}</p>
        <label className="field">
          {t("papka_karantina")}
          <input
            placeholder={t("po_umolchaniyu_na_kazhdom_ishodnom_diske")}
            value={value.quarantine || ""}
            onChange={(e) =>
              setValue({ ...value, quarantine: e.target.value || null })
            }
          />
        </label>
        <p className="muted">{ui.quarantineFolderHelp}</p>
        <p className="muted">
          {t(
            "vybrannaya_papka_dolzhna_suschestvovat_perenos_na_drugoy_disk_bud",
          )}
        </p>
      </section>
      <section className="panel">
        <h3>{t("porogi_po_umolchaniyu")}</h3>
        <div className="form-grid">
          {field("phash_max", t("rasstoyanie_phash"))}
          {field("ssim_min", t("minimalnyy_ssim"), 0.01)}
          {field("min_size", t("minimalnyy_razmer_bayt"))}
          {field("series_gap_secs", t("razryv_serii_sekund"))}
          {field("event_gap_secs", t("razryv_sobytiya_sekund"))}
        </div>
      </section>
      <section className="panel">
        <h3>{t("nagruzka_na_mashinu")}</h3>
        <div className="form-grid">
          <label className="field">
            {t("potokov_dekodirovaniya")}
            <input
              type="number"
              min={0}
              max={settings.cores}
              value={value.workers}
              onChange={(e) => setValue({ ...value, workers: +e.target.value })}
            />
          </label>
        </div>
        <p className="muted">
          {t("yader_dostupno", settings.cores)} {ui.workersHint}
        </p>
      </section>
      <section className="panel">
        <h3>{t("vneshniy_vid")}</h3>
        <div className="form-grid">
          <label className="field">
            {ui.language}
            <select
              value={value.language}
              onChange={(e) =>
                setValue({
                  ...value,
                  language: e.target.value as Settings["language"],
                })
              }
            >
              {LANGUAGES.map((code) => (
                <option key={code} value={code}>
                  {ui.languageNames[code]}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            {t("tema")}
            <select
              value={value.theme}
              onChange={(e) =>
                setValue({
                  ...value,
                  theme: e.target.value as Settings["theme"],
                })
              }
            >
              <option value="system">{t("kak_v_sisteme")}</option>
              <option value="light">{t("svetlaya")}</option>
              <option value="dark">{t("tyomnaya")}</option>
            </select>
          </label>
          <label className="field">
            {t("plotnost")}
            <select
              value={value.density}
              onChange={(e) =>
                setValue({
                  ...value,
                  density: e.target.value as Settings["density"],
                })
              }
            >
              <option value="comfortable">{t("svobodnaya")}</option>
              <option value="compact">{t("kompaktnaya")}</option>
            </select>
          </label>
        </div>
      </section>
      {error && <ErrorBox message={error} />}
      <div className="inline">
        <Button type="submit" kind="primary" disabled={disabled}>
          {ui.save}
        </Button>
        {saved && (
          <span className="green" role="status">
            ✓ {ui.saved}
          </span>
        )}
      </div>
      <ResetIndex disabled={disabled} onReset={onSaved} />
    </form>
  );
}

/** Start over: forget the index, keep the archive and the journal. */
function ResetIndex({
  disabled,
  onReset,
}: {
  disabled: boolean;
  onReset: () => void;
}) {
  const [word, setWord] = useState(""),
    [busy, setBusy] = useState(false),
    [done, setDone] = useState(false),
    [error, setError] = useState("");
  return (
    <section className="panel danger-panel">
      <h3>{ui.resetTitle}</h3>
      <p className="muted">{ui.resetText}</p>
      <div className="inline">
        <label className="field">
          {ui.resetPrompt}
          <input
            value={word}
            placeholder={ui.resetWord}
            onChange={(e) => {
              setWord(e.target.value);
              setDone(false);
            }}
          />
        </label>
        <Button
          kind="danger"
          type="button"
          disabled={disabled || busy || word !== ui.resetWord}
          onClick={async () => {
            setBusy(true);
            setError("");
            try {
              await post("/reset", { confirmation: word });
              setWord("");
              setDone(true);
              onReset();
            } catch (e) {
              setError((e as Error).message);
            } finally {
              setBusy(false);
            }
          }}
        >
          {ui.resetAction}
        </Button>
        {done && (
          <span className="green" role="status">
            ✓ {ui.resetDone}
          </span>
        )}
      </div>
      {error && <ErrorBox message={error} />}
    </section>
  );
}
