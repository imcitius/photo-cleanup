import { t } from "./i18n";
import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { post, useResource } from "./api";
import { Button, ErrorBox, Icon, Loading, Modal, Notice } from "./components";
import { SeriesPage, Categories } from "./curation";
import { ReviewQueue as Families, ReviewShortcuts } from "./review-queue";
import { Tree } from "./tree";
import {
  Overview,
  Setup,
  Policy,
  Derived,
  Quarantine,
  Organize,
  SettingsPage,
} from "./pages";
import { JobProgress, JobResult, JournalPage, useJobs } from "./workflow";
import { jobName, setLanguage, ui } from "./i18n";
import type { Page, Settings, Status } from "./types";
import "./style.css";
const navigation: { section: string; items: [Page, string][] }[] = [
  {
    section: t("arhiv"),
    items: [
      ["overview", "grid"],
      ["setup", "folder"],
      ["tree", "folder"],
      ["families", "layers"],
      ["series", "series"],
      ["categories", "image"],
    ],
  },
  {
    section: t("poryadok"),
    items: [
      ["plan", "list"],
      ["organize", "folder"],
      ["derived", "download"],
      ["quarantine", "archive"],
    ],
  },
  {
    section: t("sistema"),
    items: [
      ["journal", "clock"],
      ["settings", "settings"],
    ],
  },
];
const initial = () => {
  const page = location.hash.slice(1) as Page;
  return page in ui.pages ? page : "overview";
};
function App() {
  const [page, setPage] = useState<Page>(initial),
    [help, setHelp] = useState(false),
    [menuOpen, setMenuOpen] = useState(false),
    [theme, setTheme] = useState(localStorage.getItem("pc-theme") || "system");
  useEffect(() => {
    if (!menuOpen) return;
    const closeMenu = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setMenuOpen(false);
        document.querySelector<HTMLButtonElement>(".mobile-menu")?.focus();
      }
    };
    document.addEventListener("keydown", closeMenu);
    return () => document.removeEventListener("keydown", closeMenu);
  }, [menuOpen]);
  const jobs = useJobs();
  const [dismissed, setDismissed] = useState<number | null>(null);
  // Work that is over before it is read is not worth a panel: moving one
  // group's copies takes a fraction of a second, and a block appearing above
  // the page and vanishing again pushes everything down and back for no
  // reason. Anything still running after a moment gets the panel as before,
  // and the indicator in the top bar shows the rest without moving anything.
  const [showProgress, setShowProgress] = useState(false);
  const running = !!jobs.active;
  useEffect(() => {
    if (!running) {
      setShowProgress(false);
      return;
    }
    const timer = setTimeout(() => setShowProgress(true), 900);
    return () => clearTimeout(timer);
  }, [running]);
  const last = jobs.jobs[0];
  const settings = useResource<Settings>("/settings"),
    status = useResource<Status>("/status", jobs.revision);
  useEffect(() => {
    const hash = () => {
      setPage(initial());
      window.scrollTo(0, 0);
    };
    window.addEventListener("hashchange", hash);
    const keyboard = (e: KeyboardEvent) => {
      if (
        e.key === "?" &&
        !(
          e.target instanceof HTMLElement &&
          e.target.closest("input,textarea,select")
        )
      ) {
        e.preventDefault();
        setHelp(true);
      }
    };
    document.addEventListener("keydown", keyboard);
    return () => {
      window.removeEventListener("hashchange", hash);
      document.removeEventListener("keydown", keyboard);
    };
  }, []);
  useEffect(() => {
    // The language belongs to the archive, not to the browser: a machine
    // opening this for the first time should speak what the operator chose.
    // After that the two agree and this does nothing.
    if (settings.data && !localStorage.getItem("pc-lang")) {
      setLanguage(settings.data.language);
    }
    const theme =
      localStorage.getItem("pc-theme") || settings.data?.theme || "system";
    document.documentElement.dataset.theme = theme;
    setTheme(theme);
    document.documentElement.dataset.density =
      localStorage.getItem("pc-density") ||
      settings.data?.density ||
      "comfortable";
  }, [settings.data]);
  const navigate = (p: Page) => {
    location.hash = p;
  };
  const toggleTheme = () => {
    const dark =
      theme === "dark" ||
      (theme === "system" &&
        matchMedia("(prefers-color-scheme: dark)").matches);
    const next = dark ? "light" : "dark";
    setTheme(next);
    localStorage.setItem("pc-theme", next);
    document.documentElement.dataset.theme = next;
  };
  const common = {
    revision: jobs.revision,
    disabled: !!jobs.active,
    start: jobs.start,
  };
  let content;
  switch (page) {
    case "overview":
      content = status.loading ? (
        <Loading />
      ) : status.error ? (
        <ErrorBox message={status.error} retry={status.reload} />
      ) : (
        <Overview
          status={status.data}
          jobs={jobs.jobs}
          revision={jobs.revision}
          navigate={navigate}
        />
      );
      break;
    case "setup":
      content = settings.data && (
        <Setup
          key={settings.data.db_path}
          settings={settings.data}
          start={jobs.start}
          disabled={!!jobs.active}
          onSettings={settings.reload}
        />
      );
      break;
    case "tree":
      content = (
        <Tree
          revision={jobs.revision}
          disabled={!!jobs.active}
          onChange={jobs.refresh}
        />
      );
      break;
    case "families":
      content = (
        <Families
          revision={jobs.revision}
          disabled={!!jobs.active}
          onChange={jobs.refresh}
        />
      );
      break;
    case "series":
      content = (
        <SeriesPage
          revision={jobs.revision}
          disabled={!!jobs.active}
          onChange={jobs.refresh}
        />
      );
      break;
    case "categories":
      content = (
        <Categories
          revision={jobs.revision}
          disabled={!!jobs.active}
          onChange={jobs.refresh}
          start={jobs.start}
        />
      );
      break;
    case "plan":
      content = <Policy {...common} />;
      break;
    case "derived":
      content = <Derived {...common} />;
      break;
    case "quarantine":
      content = <Quarantine {...common} />;
      break;
    case "organize":
      content = settings.data && (
        <Organize {...common} settings={settings.data} />
      );
      break;
    case "journal":
      content = <JournalPage {...common} />;
      break;
    case "settings":
      content = settings.data && (
        <SettingsPage
          settings={settings.data}
          disabled={!!jobs.active}
          onSaved={settings.reload}
        />
      );
      break;
  }
  return (
    <div className="app-shell">
      <a
        href="#main"
        className="skip-link"
        onClick={(e) => {
          e.preventDefault();
          document.getElementById("main")?.focus();
        }}
      >
        {t("k_soderzhimomu")}
      </a>
      <aside className={`sidebar ${menuOpen ? "menu-open" : ""}`}>
        <a
          className="brand"
          href="#overview"
          onClick={() => {
            setMenuOpen(false);
            if (menuOpen) document.getElementById("main")?.focus();
          }}
        >
          <span className="brand-mark">
            <Icon name="layers" size={23} />
          </span>
          <span>
            photo-cleanup<small>{t("berezhno_k_kazhdomu_snimku")}</small>
          </span>
        </a>
        <Button
          kind="icon-button mobile-menu"
          icon={menuOpen ? "close" : "list"}
          aria-label={t("osnovnaya_navigatsiya")}
          aria-expanded={menuOpen}
          aria-controls="archive-navigation"
          onClick={() => setMenuOpen(!menuOpen)}
        />
        <div className="workspace-label">
          <span className="workspace-dot" />
          {ui.local}
        </div>
        <nav id="archive-navigation" aria-label={t("osnovnaya_navigatsiya")}>
          {navigation.map((group) => (
            <div className="nav-group" key={group.section}>
              <span className="nav-label">{group.section}</span>
              {group.items.map(([key, icon]) => (
                <a
                  href={`#${key}`}
                  onClick={() => {
                    setMenuOpen(false);
                    if (menuOpen) document.getElementById("main")?.focus();
                  }}
                  key={key}
                  className={page === key ? "active" : ""}
                  aria-current={page === key ? "page" : undefined}
                >
                  <Icon name={icon} size={19} />
                  <span>{ui.pages[key]}</span>
                  {key === "quarantine" && !!status.data?.quarantined_bytes && (
                    <span className="nav-dot" />
                  )}
                </a>
              ))}
            </div>
          ))}
        </nav>
        <div className="sidebar-bottom">
          <div>
            <Icon name="shield" size={18} />
            <span>
              {t("vash_arhiv_ostayotsya_u_vas")}
              <small>{ui.noInternet}</small>
            </span>
          </div>
          <span className="version">
            PHOTO-CLEANUP <span>v{status.data?.version ?? "…"}</span>
          </span>
        </div>
      </aside>
      <div className="workspace">
        <header className="topbar">
          <div className="breadcrumb">
            {ui.archive}
            <span>/</span>
            <strong>{ui.pages[page]}</strong>
          </div>
          <div className="inline">
            {/* What the server is doing, on every page and at every scroll
                position: the one question a long job leaves unanswered. */}
            {jobs.active ? (
              <button
                type="button"
                className="activity running"
                title={t("pokazat_tekuschuyu_zadachu")}
                onClick={() =>
                  document
                    .getElementById("job-progress")
                    ?.scrollIntoView({ behavior: "smooth", block: "center" })
                }
              >
                <span className="spinner" />
                <strong>{jobName(jobs.active.kind)}</strong>
                {!!jobs.active.progress.steps && (
                  <span>
                    {t(
                      "shag_iz",
                      jobs.active.progress.step,
                      jobs.active.progress.steps,
                    )}
                  </span>
                )}
                {!!jobs.active.progress.total && (
                  <span>
                    {Math.min(
                      100,
                      Math.round(
                        ((jobs.active.progress.done || 0) * 100) /
                          jobs.active.progress.total,
                      ),
                    )}
                    %
                  </span>
                )}
              </button>
            ) : (
              <span className="activity">{t("nichego_ne_vypolnyaetsya")}</span>
            )}
            <span className="connection">
              <span className={`status-dot ${status.error ? "offline" : ""}`} />
              {settings.data?.network
                ? t("dostupen_po_seti")
                : t("na_etom_kompyutere")}
            </span>
            <Button
              kind="icon-button"
              icon="sun"
              onClick={toggleTheme}
              aria-label={t("pereklyuchit_temu")}
            />
            <Button
              kind="icon-button"
              onClick={() => setHelp(true)}
              aria-label={ui.keyboard}
            >
              ?
            </Button>
          </div>
        </header>
        <main id="main" tabIndex={-1}>
          <div className="page-heading">
            <div>
              <div className="eyebrow">
                {
                  navigation.find((group) =>
                    group.items.some(([key]) => key === page),
                  )?.section
                }
              </div>
              <h1>{ui.pages[page]}</h1>
              <p>{ui.subtitles[page]}</p>
            </div>
            <Button
              icon="clock"
              onClick={() => {
                jobs.refresh();
                status.reload();
              }}
            >
              {ui.refresh}
            </Button>
          </div>
          {jobs.error && <Notice tone="warning">{jobs.error}</Notice>}
          {jobs.active && showProgress && (
            <JobProgress
              id="job-progress"
              job={jobs.active}
              onCancel={async () => {
                await post(`/jobs/${jobs.active!.id}/cancel`);
                jobs.refresh();
              }}
            />
          )}
          {last &&
            !jobs.active &&
            last.id !== dismissed &&
            (last.state === "failed" ||
              (last.state === "interrupted" && page !== "overview") ||
              !!last.progress.refusals?.length) && (
              <JobResult job={last} onDismiss={() => setDismissed(last.id)} />
            )}
          {settings.error && (
            <ErrorBox message={settings.error} retry={settings.reload} />
          )}{" "}
          {!content && settings.loading ? <Loading /> : content}
          <footer className="page-footer">
            <span>
              <Icon name="shield" size={14} />
              {t("snachala_plan_zatem_deystvie")}
            </span>
            <span>{t("photo_cleanup_lokalno_na_vashem_servere")}</span>
          </footer>
        </main>
      </div>
      {help && (
        <Modal title={ui.keyboard} onClose={() => setHelp(false)}>
          {page === "families" && <ReviewShortcuts />}
          <dl className="keyboard-help">
            <div>
              <dt>
                <kbd>J</kbd> / <kbd>K</kbd>
              </dt>
              <dd>
                {t("rq_previous")} / {t("rq_next")}
              </dd>
            </div>
            <div>
              <dt>
                <kbd>Space</kbd>
              </dt>
              <dd>{t("sdelat_vybrannyy_fayl_hranimym")}</dd>
            </div>
            <div>
              <dt>
                <kbd>Enter</kbd>
              </dt>
              <dd>{t("otkryt_vybrannyy_kadr_krupno")}</dd>
            </div>
            <div>
              <dt>
                <kbd>/</kbd>
              </dt>
              <dd>{t("poisk_v_semeystvah")}</dd>
            </div>
            <div>
              <dt>
                <kbd>Esc</kbd>
              </dt>
              <dd>{t("zakryt_okno_i_vernut_fokus")}</dd>
            </div>
            <div>
              <dt>
                <kbd>Tab</kbd>
              </dt>
              <dd>{t("pereyti_k_sleduyuschemu_deystviyu")}</dd>
            </div>
          </dl>
        </Modal>
      )}
    </div>
  );
}
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
