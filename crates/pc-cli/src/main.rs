//! `photo-cleanup` — command line entry point.

use pc_cli::{format, scan};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use pc_core::{fmt_bytes, DerivedKind};
use pc_db::{BundleState, Db};
use std::path::PathBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(
    name = "photo-cleanup",
    version = VERSION,
    about = "Sort out a photo archive: duplicates, bursts, regenerable data"
)]
struct Cli {
    /// Path to the database. Defaults to ./photo-cleanup.db
    #[arg(long, global = true, default_value = "photo-cleanup.db")]
    db: PathBuf,

    #[arg(long, global = true, default_value = "info")]
    log: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Walk the roots and take stock of derived data
    Scan(ScanArgs),
    /// Read the images and build the index
    Index(IndexArgs),
    /// Regenerable data: Lightroom previews, caches, system junk
    #[command(subcommand)]
    Derived(DerivedCmd),
    /// Groups: one photograph in several renditions
    #[command(subcommand)]
    Families(FamiliesCmd),
    /// Sort by kind: documents, screenshots, empty frames
    #[command(subcommand)]
    Categories(CategoriesCmd),
    /// Bursts: several frames of one moment, and the best of them
    #[command(subcommand)]
    Series(SeriesCmd),
    /// What the current policy would move
    Plan(PolicyArgs),
    /// Move to quarantine, following the plan
    Apply(ApplyArgs),
    /// Sort the archive by date: YYYY/YYYY-MM-DD_event
    #[command(subcommand)]
    Organize(OrganizeCmd),
    /// Start the web interface
    Serve(ServeArgs),
    /// Summary of the database
    Status,
    /// Lightroom catalogues that were found
    Catalogs,
}

#[derive(Args)]
struct ScanArgs {
    /// Root to walk. Use /mnt/diskN/..., not /mnt/user/...
    #[arg(long = "root", required = true)]
    roots: Vec<PathBuf>,
}

#[derive(Args)]
struct PolicyArgs {
    /// Role to remove; repeatable. Only `copy` by default.
    #[arg(long = "role")]
    roles: Vec<String>,
    /// A `resize` is removed only when the frame is smaller than this
    #[arg(long, default_value = "2M", value_parser = format::parse_size)]
    resize_below: i64,
    /// Lift the protection on files a Lightroom catalogue references
    #[arg(long)]
    allow_lightroom: bool,
    /// Show every refusal, not just the first few
    #[arg(long)]
    show_refusals: bool,
}

#[derive(Args)]
struct ApplyArgs {
    #[command(flatten)]
    policy: PolicyArgs,
    #[arg(long)]
    quarantine: Option<PathBuf>,
    #[arg(long)]
    yes: bool,
}

#[derive(Subcommand)]
enum OrganizeCmd {
    /// Show what moves, and where
    Plan(OrganizeArgs),
    /// Carry out the moves
    Apply(OrganizeApplyArgs),
    /// Put the files of a run back where they came from
    Undo(OrganizeUndoArgs),
    /// Runs that moved something
    Runs,
}

#[derive(Args)]
struct OrganizeArgs {
    /// Root of the new tree. Has to be on the same disk as the files.
    #[arg(long)]
    root: PathBuf,
    /// Gap between shoots that starts a new event
    #[arg(long, default_value = "6h", value_parser = format::parse_duration)]
    gap: i64,
    /// Move files a Lightroom catalogue references as well
    #[arg(long)]
    allow_lightroom: bool,
    /// Leave files whose date is a guess (from the path or mtime)
    #[arg(long)]
    skip_uncertain: bool,
    /// Sort without waiting for the duplicates to be resolved
    #[arg(long)]
    allow_duplicates: bool,
    /// How many moves to print
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

#[derive(Args)]
struct OrganizeApplyArgs {
    #[command(flatten)]
    opts: OrganizeArgs,
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
struct OrganizeUndoArgs {
    /// Run number; defaults to the last one that moved anything
    #[arg(long)]
    run: Option<i64>,
    #[arg(long)]
    yes: bool,
}

#[derive(Subcommand)]
enum CategoriesCmd {
    /// Classify everything in the index
    Build,
    /// Summary by kind
    List,
    /// Show the files of one kind
    Show(CategoryShowArgs),
}

#[derive(Args)]
struct CategoryShowArgs {
    /// document, screenshot, blank, monochrome, photo
    category: String,
    #[arg(long, default_value_t = 25)]
    limit: i64,
}

#[derive(Subcommand)]
enum SeriesCmd {
    /// Find bursts and rank their frames
    Build(SeriesBuildArgs),
    /// Show the bursts
    List(SeriesListArgs),
}

#[derive(Args)]
struct SeriesBuildArgs {
    /// Largest gap between frames of one burst, in seconds
    #[arg(long, default_value_t = 10)]
    gap: i64,
}

#[derive(Args)]
struct SeriesListArgs {
    #[arg(long, default_value_t = 10)]
    limit: i64,
    #[arg(long, default_value_t = 0)]
    offset: i64,
    #[arg(long, short)]
    verbose: bool,
}

#[derive(Args)]
struct ServeArgs {
    /// Open the interface in a browser once the server is listening
    #[arg(long)]
    open: bool,
    /// Address. 0.0.0.0 to reach it from other machines.
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: String,
    #[arg(long)]
    thumbs: Option<PathBuf>,
    /// Where moved files go when the disk root is not writable
    #[arg(long)]
    quarantine: Option<PathBuf>,
}

#[derive(Args)]
struct IndexArgs {
    #[arg(long = "root", required = true)]
    roots: Vec<PathBuf>,
    /// Thumbnail cache directory. Next to the database by default.
    #[arg(long)]
    thumbs: Option<PathBuf>,
    /// Smallest file to consider; below this are icons and assets
    #[arg(long, default_value = "100K", value_parser = format::parse_size)]
    min_size: i64,
    /// Readers per physical disk. More than two hurts on a spinning disk.
    #[arg(long, default_value_t = 2)]
    readers_per_disk: usize,
    /// Re-read even what is already indexed
    #[arg(long)]
    reindex: bool,
    /// Decoding threads. 0 means one per core, less one for the rest of the machine.
    #[arg(long, default_value_t = 0)]
    workers: usize,
}

#[derive(Subcommand)]
enum FamiliesCmd {
    /// Build the groups from links and from similarity
    Build(BuildArgs),
    /// Show the groups
    List(FamListArgs),
}

#[derive(Args)]
struct BuildArgs {
    #[arg(long)]
    thumbs: Option<PathBuf>,
    /// pHash distance threshold for candidates
    #[arg(long, default_value_t = 10)]
    phash_max: u32,
    /// Smallest SSIM at which a pair counts as one photograph
    #[arg(long, default_value_t = 0.90)]
    ssim_min: f64,
}

#[derive(Args)]
struct FamListArgs {
    #[arg(long, default_value_t = 20)]
    limit: i64,
    #[arg(long, default_value_t = 0)]
    offset: i64,
    /// Show single frames too
    #[arg(long)]
    all: bool,
    /// Paths, links and the score breakdown
    #[arg(long, short)]
    verbose: bool,
}

#[derive(Subcommand)]
enum DerivedCmd {
    /// Show the bundles that were found
    List(ListArgs),
    /// Move to quarantine
    Clean(CleanArgs),
    /// Delete from quarantine, for good
    Purge(PurgeArgs),
    /// Bring back from quarantine by journal entry
    Undo(UndoArgs),
}

#[derive(Args)]
struct ListArgs {
    #[arg(long)]
    kind: Option<String>,
    /// Smallest size, for example 100M
    #[arg(long, value_parser = format::parse_size)]
    min_size: Option<i64>,
    /// Show blocked and already-moved bundles too
    #[arg(long)]
    all: bool,
}

#[derive(Args)]
struct CleanArgs {
    /// Kind of data; repeatable. lr-previews by default
    #[arg(long = "kind")]
    kinds: Vec<String>,
    #[arg(long, value_parser = format::parse_size)]
    min_size: Option<i64>,
    /// Only show what would be done
    #[arg(long)]
    dry_run: bool,
    /// Confirm and carry it out
    #[arg(long)]
    yes: bool,
    /// Where quarantine goes. By default, beside each file.
    /// The path has to be on the same filesystem as the data.
    #[arg(long)]
    quarantine: Option<PathBuf>,
}

#[derive(Args)]
struct PurgeArgs {
    /// Holding period, for example 7d
    #[arg(long, default_value = "7d", value_parser = format::parse_duration)]
    older_than: i64,
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
struct UndoArgs {
    #[arg(long)]
    journal: i64,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| cli.log.clone().into()),
        )
        .with_target(false)
        .without_time()
        .init();

    let db = Db::open(&cli.db)?;

    match cli.command {
        Command::Scan(a) => scan::run(&db, &a.roots, VERSION),
        Command::Index(a) => {
            let store = pc_core::ThumbStore::new(thumbs_dir(&cli.db, a.thumbs));
            let summary = pc_cli::index::run(
                &db,
                &a.roots,
                &store,
                &pc_cli::index::Options {
                    min_file_size: a.min_size.max(0) as u64,
                    readers_per_disk: a.readers_per_disk.max(1),
                    reindex: a.reindex,
                    workers: a.workers,
                },
            )?;
            println!("\n{}", summary.report());
            print_index_breakdown(&db)?;
            Ok(())
        }
        Command::Derived(DerivedCmd::List(a)) => cmd_list(&db, a),
        Command::Derived(DerivedCmd::Clean(a)) => cmd_clean(&db, a),
        Command::Derived(DerivedCmd::Purge(a)) => cmd_purge(&db, a),
        Command::Derived(DerivedCmd::Undo(a)) => {
            pc_apply::undo(&db, a.journal)?;
            println!("Entry {} rolled back.", a.journal);
            Ok(())
        }
        Command::Families(FamiliesCmd::Build(a)) => {
            let store = pc_core::ThumbStore::new(thumbs_dir(&cli.db, a.thumbs));
            let params = pc_family::Params {
                phash_max: a.phash_max,
                ssim_min: a.ssim_min,
                ..Default::default()
            };
            let r = pc_family::build(&db, &store, &params)?;
            println!(
                "Файлов {}, семейств {} (с несколькими файлами {}).\n\
                 Точных связей {}, кандидатов по сходству {}, подтверждено {}.\n\
                 Отклонено: по SSIM {}, как разные кадры серии {}, как однородные {}.",
                r.files,
                r.families,
                r.multi_member,
                r.exact_links,
                r.perceptual_candidates,
                r.perceptual_verified,
                r.rejected_by_ssim,
                r.rejected_as_series,
                r.rejected_as_blank
            );
            pc_cli::families::print_summary(&db)?;
            Ok(())
        }
        Command::Families(FamiliesCmd::List(a)) => {
            let fams = db.families(!a.all, a.limit, a.offset)?;
            if fams.is_empty() {
                println!("Nothing to show. Run `families build` first.");
                return Ok(());
            }
            for f in &fams {
                pc_cli::families::print_family(f, a.verbose);
            }
            pc_cli::families::print_summary(&db)?;
            Ok(())
        }
        Command::Serve(a) => {
            let thumbs = thumbs_dir(&cli.db, a.thumbs);
            let addr: std::net::SocketAddr = a
                .bind
                .parse()
                .with_context(|| format!("cannot parse the address “{}”", a.bind))?;
            // The database is reopened inside the server, so release ours.
            drop(db);
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(pc_api::serve(&cli.db, &thumbs, a.quarantine, addr, a.open))
        }
        Command::Categories(CategoriesCmd::Build) => {
            let r = pc_family::categories::build(&db)?;
            println!("Classified: {}", r.classified);
            for (label, n) in &r.by_category {
                println!("  {label}: {n}");
            }
            println!(
                "\nСемантические виды — «фото счётчика», «чек» — здесь не определяются:\n\
                 это вопрос о смысле картинки, а не о её пикселях, и нужна модель."
            );
            Ok(())
        }
        Command::Categories(CategoriesCmd::List) => {
            let rows = db.category_counts()?;
            if rows.is_empty() {
                println!("Nothing classified. Run `categories build` first.");
                return Ok(());
            }
            for c in rows {
                let label = pc_family::categories::Category::parse(&c.category)
                    .map(|x| x.label())
                    .unwrap_or("other");
                println!(
                    "  {:<22} {:>8}  {:>10}",
                    label,
                    pc_core::count_en(c.count, "file", "files"),
                    fmt_bytes(c.bytes as u64)
                );
            }
            Ok(())
        }
        Command::Categories(CategoriesCmd::Show(a)) => {
            let rows = db.files_in_category(&a.category, a.limit)?;
            if rows.is_empty() {
                println!("Nothing of kind “{}”.", a.category);
                return Ok(());
            }
            for f in rows {
                println!(
                    "  {:>5.0}%  {:<40} {:>10}  {}",
                    f.quality * 100.0,
                    format!("{}×{}", f.width, f.height),
                    fmt_bytes(f.size as u64),
                    f.path
                );
                if !f.breakdown.is_empty() {
                    println!("         {}", f.breakdown);
                }
            }
            Ok(())
        }
        Command::Series(SeriesCmd::Build(a)) => {
            let r = pc_family::series::build(&db, a.gap)?;
            println!("Bursts: {}, frames in them: {}.", r.series, r.frames);
            for (kind, n) in &r.by_kind {
                println!("  {kind}: {n}");
            }
            if r.protected > 0 {
                println!(
                    "\n{} защищено от прореживания (pixel-shift: один снимок, хранится \
                     несколькими файлами).",
                    r.protected
                );
            }
            Ok(())
        }
        Command::Series(SeriesCmd::List(a)) => {
            let rows = db.series_list(a.limit, a.offset)?;
            if rows.is_empty() {
                println!("No bursts. Run `photo-cleanup series build` first.");
                return Ok(());
            }
            for s in &rows {
                pc_cli::families::print_series(s, a.verbose);
            }
            println!("\nBursts in total: {}", db.series_count()?);
            Ok(())
        }
        Command::Plan(a) => cmd_plan(&db, &a, None, false),
        Command::Apply(a) => {
            let q = a.quarantine.clone();
            cmd_plan(&db, &a.policy, q.as_deref(), a.yes)
        }
        Command::Organize(OrganizeCmd::Plan(a)) => cmd_organize(&db, &a, false),
        Command::Organize(OrganizeCmd::Apply(a)) => cmd_organize(&db, &a.opts, a.yes),
        Command::Organize(OrganizeCmd::Undo(a)) => cmd_organize_undo(&db, &a),
        Command::Organize(OrganizeCmd::Runs) => cmd_organize_runs(&db),
        Command::Status => cmd_status(&db),
        Command::Catalogs => cmd_catalogs(&db),
    }
}

/// Thumbnails live beside the database unless told otherwise.
fn thumbs_dir(db: &std::path::Path, given: Option<PathBuf>) -> PathBuf {
    given.unwrap_or_else(|| {
        db.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."))
            .join("thumbs")
    })
}

fn parse_kind(s: &str) -> Result<DerivedKind> {
    DerivedKind::parse(s).with_context(|| {
        format!(
            "неизвестный вид «{s}». Доступно: lr-previews, lr-smart-previews, \
             lr-helper, lr-lrdata-other, system-junk"
        )
    })
}

fn cmd_list(db: &Db, a: ListArgs) -> Result<()> {
    let filter = pc_db::model::BundleFilter {
        kind: a.kind.as_deref().map(parse_kind).transpose()?,
        state: (!a.all).then_some(BundleState::Present),
        min_size: a.min_size,
        removable_only: false,
    };
    let bundles = db.list_bundles(&filter)?;
    if bundles.is_empty() {
        println!("Nothing found. Run `photo-cleanup scan --root ...` first.");
        return Ok(());
    }
    format::print_grouped(&bundles);
    Ok(())
}

fn cmd_clean(db: &Db, a: CleanArgs) -> Result<()> {
    let kinds: Vec<DerivedKind> = if a.kinds.is_empty() {
        vec![DerivedKind::LrPreviews]
    } else {
        a.kinds
            .iter()
            .map(|s| parse_kind(s))
            .collect::<Result<Vec<_>>>()?
    };

    for k in &kinds {
        if !k.regenerable() {
            bail!(
                "kind “{}” ({}) is never removed: nothing regenerates it",
                k.as_str(),
                k.label()
            );
        }
    }

    let mut selected = Vec::new();
    for k in &kinds {
        let f = pc_db::model::BundleFilter {
            kind: Some(*k),
            state: Some(BundleState::Present),
            min_size: a.min_size,
            removable_only: true,
        };
        selected.extend(db.list_bundles(&f)?);
    }

    if selected.is_empty() {
        println!("Nothing matches those conditions.");
        return Ok(());
    }

    let files: i64 = selected.iter().map(|b| b.file_count).sum();
    let bytes: i64 = selected.iter().map(|b| b.size).sum();
    println!(
        "To move to quarantine: {}, {}, {}\n",
        pc_core::count_en(selected.len() as i64, "bundle", "bundles"),
        pc_core::count_en(files, "file", "files"),
        fmt_bytes(bytes as u64)
    );
    format::print_grouped_opts(&selected, false);

    if a.dry_run {
        println!("\n--dry-run: nothing changed.");
        return Ok(());
    }
    if !a.yes {
        println!("\nAdd --yes to carry it out.");
        return Ok(());
    }

    let run_id = db.latest_run()?.context("no runs yet; run scan first")?;
    let totals = pc_apply::quarantine_many(db, run_id, &selected, a.quarantine.as_deref())?;

    println!("\nMoved: {}", totals.summary());
    for s in &totals.skipped {
        println!("  skipped: {s}");
    }
    println!(
        "\nМесто пока не освободилось — данные лежат в карантине.\n\
         Освободить: photo-cleanup derived purge --older-than 7d --yes"
    );
    Ok(())
}

fn cmd_purge(db: &Db, a: PurgeArgs) -> Result<()> {
    let pending = db.journal_quarantined(Some(pc_core::time::now_unix() - a.older_than))?;
    if pending.is_empty() {
        println!("Nothing to delete: no quarantine entry is older than that.");
        return Ok(());
    }
    let files: i64 = pending.iter().map(|e| e.file_count).sum();
    let bytes: i64 = pending.iter().map(|e| e.size).sum();

    println!(
        "Will be deleted irreversibly: {}, {}, {}",
        pc_core::count_en(pending.len() as i64, "object", "objects"),
        pc_core::count_en(files, "file", "files"),
        fmt_bytes(bytes as u64)
    );
    for e in pending.iter().take(10) {
        println!("  {}", e.dst.as_deref().unwrap_or(&e.src));
    }
    if pending.len() > 10 {
        println!("  … and {} more", pending.len() - 10);
    }

    if !a.yes {
        println!("\nThis cannot be undone. Add --yes to carry it out.");
        return Ok(());
    }
    let totals = pc_apply::purge(db, a.older_than)?;
    println!("\nDeleted: {}", totals.summary());
    for s in &totals.skipped {
        println!("  skipped: {s}");
    }
    Ok(())
}

fn print_index_breakdown(db: &Db) -> Result<()> {
    let rows = db.container_counts()?;
    if rows.is_empty() {
        return Ok(());
    }
    println!("\nBy container:");
    for (name, count, bytes) in rows {
        println!(
            "  {:<16} {:>8}  {:>10}",
            name,
            pc_core::count_en(count, "file", "files"),
            fmt_bytes(bytes as u64)
        );
    }
    let lied = db.mislabelled_count()?;
    if lied > 0 {
        println!(
            "\n{} — the extension disagreed with the contents.",
            pc_core::count_en(lied, "file", "files")
        );
    }
    Ok(())
}

fn build_policy(a: &PolicyArgs) -> Result<pc_family::Policy> {
    let mut p = pc_family::Policy {
        resize_below_pixels: a.resize_below.max(0),
        respect_lightroom: !a.allow_lightroom,
        ..Default::default()
    };
    if !a.roles.is_empty() {
        p.remove_roles = a
            .roles
            .iter()
            .map(|s| {
                pc_family::Role::parse(s).with_context(|| {
                    format!(
                        "неизвестная роль «{s}». Доступно: copy, resize, export, \
                         converted, camera-jpg, unknown"
                    )
                })
            })
            .collect::<Result<_>>()?;
    }
    if p.remove_roles.contains(&pc_family::Role::Original) {
        bail!("the original role is never removed: it is the photograph itself");
    }
    Ok(p)
}

fn cmd_plan(
    db: &Db,
    args: &PolicyArgs,
    quarantine: Option<&std::path::Path>,
    execute: bool,
) -> Result<()> {
    let policy = build_policy(args)?;
    let plan = pc_family::plan::compute(db, &policy)?;

    let roles: Vec<&str> = policy.remove_roles.iter().map(|r| r.as_str()).collect();
    println!(
        "Policy: removing roles [{}]{}\n",
        roles.join(", "),
        if policy.respect_lightroom {
            ", files in Lightroom catalogues are protected"
        } else {
            ", LIGHTROOM PROTECTION LIFTED"
        }
    );

    if plan.candidates.is_empty() {
        println!("Nothing falls under that policy.");
    } else {
        println!(
            "To move: {}, {}\n",
            pc_core::count_en(plan.candidates.len() as i64, "file", "files"),
            fmt_bytes(plan.bytes() as u64)
        );
        for c in plan.candidates.iter().take(15) {
            println!("  {:>10}  {}", fmt_bytes(c.size as u64), c.path);
            println!("              {}", c.reason);
        }
        if plan.candidates.len() > 15 {
            println!("  … and {} more", plan.candidates.len() - 15);
        }
    }

    if !plan.refusals.is_empty() {
        println!(
            "\nProtected from moving: {}",
            pc_core::count_en(plan.refusals.len() as i64, "file", "files")
        );
        let show = if args.show_refusals {
            plan.refusals.len()
        } else {
            8
        };
        for r in plan.refusals.iter().take(show) {
            println!("  {} — {}", r.path, r.why);
        }
        if plan.refusals.len() > show {
            println!(
                "  … and {} more (--show-refusals)",
                plan.refusals.len() - show
            );
        }
    }

    if !execute {
        if !plan.candidates.is_empty() {
            println!("\nTo carry it out: photo-cleanup apply --yes");
        }
        return Ok(());
    }

    let run_id = db
        .latest_run()?
        .context("no runs yet; run scan or index first")?;
    println!("\nChecking every file before it moves…");
    let report = pc_apply::apply(db, run_id, &plan.candidates, quarantine)?;
    println!("Moved: {}", report.totals.summary());
    for (path, why) in &report.refused {
        println!("  refused: {path} — {why}");
    }
    println!(
        "\nМесто пока не освободилось — файлы в карантине.\n\
         Вернуть: photo-cleanup derived undo --journal <id>\n\
         Освободить: photo-cleanup derived purge --older-than 7d --yes"
    );
    Ok(())
}

/// Reorganisation runs after deduplication, never before it: laying copies
/// out by date only spreads them across a tidy tree.
fn refuse_until_deduplicated(db: &Db) -> Result<()> {
    let dup = pc_family::plan::compute(db, &pc_family::Policy::default())?;
    if dup.candidates.is_empty() {
        return Ok(());
    }
    bail!(
        "сначала разбор дубликатов: под перенос в карантин подходит {} ({}).\n\
         Реорганизация до дедупа разложит по новому дереву и копии тоже.\n\
         Выполните `photo-cleanup plan`, затем `apply --yes` — либо, если так и задумано, \
         добавьте --allow-duplicates.",
        pc_core::count_en(dup.candidates.len() as i64, "file", "files"),
        fmt_bytes(dup.bytes() as u64)
    )
}

fn rel_to<'a>(path: &'a str, root: &std::path::Path) -> &'a str {
    let root = root.to_string_lossy();
    path.strip_prefix(root.as_ref())
        .map(|p| p.trim_start_matches('/'))
        .unwrap_or(path)
}

fn cmd_organize(db: &Db, a: &OrganizeArgs, execute: bool) -> Result<()> {
    if !a.allow_duplicates {
        refuse_until_deduplicated(db)?;
    }

    let opts = pc_organize::Options {
        root: a.root.clone(),
        gap_secs: a.gap.max(60),
        respect_lightroom: !a.allow_lightroom,
        skip_uncertain: a.skip_uncertain,
    };
    let plan = pc_organize::compute(db, &opts)?;

    println!(
        "Tree: {}\nAn event is a gap of more than {} h.{}\n",
        a.root.display(),
        a.gap as f64 / 3600.0,
        if opts.respect_lightroom {
            " Files in Lightroom catalogues are left alone."
        } else {
            " LIGHTROOM PROTECTION LIFTED: catalogue links will break."
        }
    );

    if plan.moves.is_empty() {
        println!("Nothing to move.");
        if plan.already_placed > 0 {
            println!(
                "  {} are already where they belong.",
                pc_core::count_en(plan.already_placed as i64, "file", "files")
            );
        }
        print_refusals(&plan);
        return Ok(());
    }

    println!(
        "To move: {}, {}\nEvents: {}{}{}",
        pc_core::count_en(plan.moves.len() as i64, "file", "files"),
        fmt_bytes(plan.bytes() as u64),
        plan.events,
        if plan.already_placed > 0 {
            format!("; already in place: {}", plan.already_placed)
        } else {
            String::new()
        },
        if plan.renamed > 0 {
            format!("; renamed because of name clashes: {}", plan.renamed)
        } else {
            String::new()
        }
    );

    println!("\nWhere the date came from:");
    for (source, n) in &plan.by_source {
        println!("  {:<18} {:>8}", source.label(), n);
    }
    if plan.uncertain > 0 {
        println!(
            "\n{} датированы не по съёмке: имя файла, путь или mtime.\n\
             Те, чья дата известна лишь до месяца или года, лежат отдельной папкой,\n\
             а не притворяются конкретным днём. Исключить их совсем: --skip-uncertain",
            pc_core::count_en(plan.uncertain as i64, "file", "files")
        );
    }

    println!("\nExample moves:");
    for m in plan.moves.iter().take(a.limit) {
        println!("  {:<44} ← {}", rel_to(&m.dst, &a.root), m.src);
        if let Some(old) = &m.renamed_from {
            println!("      name taken, was {old}");
        }
    }
    if plan.moves.len() > a.limit {
        println!("  … and {} more", plan.moves.len() - a.limit);
    }
    print_refusals(&plan);

    if !execute {
        println!(
            "\nNothing changed. To carry it out: \n  photo-cleanup organize apply --root {} --yes",
            a.root.display()
        );
        return Ok(());
    }

    let run_id = db.latest_run()?.context("no runs yet; run scan first")?;
    let report = pc_apply::organize(db, run_id, &plan.moves)?;

    println!(
        "\nMoved: {}, {}{}{}",
        pc_core::count_en(report.moved as i64, "file", "files"),
        fmt_bytes(report.bytes),
        if report.sidecars > 0 {
            format!(", companions {}", report.sidecars)
        } else {
            String::new()
        },
        if report.pruned_dirs > 0 {
            format!(", emptied directories removed {}", report.pruned_dirs)
        } else {
            String::new()
        }
    );
    for (path, why) in report.refused.iter().take(10) {
        println!("  not moved: {path} — {why}");
    }
    if report.refused.len() > 10 {
        println!("  … and {} more", report.refused.len() - 10);
    }
    println!("\nTo put it all back: photo-cleanup organize undo --run {run_id} --yes");
    Ok(())
}

fn print_refusals(plan: &pc_organize::Plan) {
    if plan.refusals.is_empty() {
        return;
    }
    println!(
        "\nLeaving {} alone:",
        pc_core::count_en(plan.refusals.len() as i64, "file", "files")
    );
    let mut by_reason: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for r in &plan.refusals {
        // The reason starts with what it is about; the tail carries the path
        // or the rating and would splinter the count.
        let head = r.why.split(" — ").next().unwrap_or(&r.why);
        *by_reason.entry(head).or_default() += 1;
    }
    for (why, n) in by_reason {
        println!("  {n:>8}  {why}");
    }
}

fn cmd_organize_undo(db: &Db, a: &OrganizeUndoArgs) -> Result<()> {
    let runs = db.organize_runs()?;
    let run_id = match a.run {
        Some(id) => id,
        None => runs
            .first()
            .map(|(id, ..)| *id)
            .context("no run has sorted anything")?,
    };
    let moved = runs
        .iter()
        .find(|(id, ..)| *id == run_id)
        .map(|(_, n, _)| *n)
        .unwrap_or(0);

    println!(
        "Run {run_id}: {} will go back where they came from.",
        pc_core::count_en(moved, "file", "files")
    );
    if !a.yes {
        println!("Add --yes to carry it out.");
        return Ok(());
    }
    let (back, failed) = pc_apply::undo_run(db, run_id)?;
    println!("Restored: {back}.");
    for f in failed.iter().take(10) {
        println!("  failed: {f}");
    }
    if failed.len() > 10 {
        println!("  … and {} more", failed.len() - 10);
    }
    Ok(())
}

fn cmd_organize_runs(db: &Db) -> Result<()> {
    let runs = db.organize_runs()?;
    if runs.is_empty() {
        println!("Nothing has been sorted yet.");
        return Ok(());
    }
    for (id, n, at) in runs {
        println!(
            "  run {id:<4} {:>8}  {}",
            pc_core::count_en(n, "file", "files"),
            pc_core::time::fmt_datetime_ru(at)
        );
    }
    Ok(())
}

fn cmd_status(db: &Db) -> Result<()> {
    let all = db.list_bundles(&pc_db::model::BundleFilter::default())?;
    let present: Vec<_> = all
        .iter()
        .filter(|b| b.state == BundleState::Present)
        .collect();
    let removable: Vec<_> = present.iter().filter(|b| b.removable()).collect();
    let blocked: Vec<_> = present.iter().filter(|b| !b.removable()).collect();

    let sum = |v: &[&&pc_db::Bundle]| -> u64 { v.iter().map(|b| b.size as u64).sum() };

    println!("Bundles in the inventory: {}", all.len());
    println!(
        "  can be moved:          {:>4}  {}",
        removable.len(),
        fmt_bytes(sum(&removable))
    );
    println!(
        "  blocked:               {:>4}  {}",
        blocked.len(),
        fmt_bytes(sum(&blocked))
    );

    let idx = db.index_stats()?;
    if idx.total > 0 {
        println!(
            "\nFiles in the index:      {}\n  images:                {:>4}\n  skipped:               {:>4}",
            idx.total, idx.images, idx.skipped
        );
    }

    let q = pc_apply::quarantined_totals(db)?;
    println!("\nIn quarantine:           {}", q.summary());
    let pend = db.journal_pending()?;
    if !pend.is_empty() {
        println!(
            "\nWARNING: {} unfinished journal entries — a run was interrupted.",
            pend.len()
        );
        for e in pend.iter().take(5) {
            println!("  #{} {} {}", e.id, e.op, e.src);
        }
    }
    Ok(())
}

fn cmd_catalogs(db: &Db) -> Result<()> {
    let cats = db.all_catalogs()?;
    if cats.is_empty() {
        println!("No Lightroom catalogues found.");
        return Ok(());
    }
    for c in &cats {
        let mut tags = Vec::new();
        if c.is_backup {
            tags.push("backup".to_string());
        }
        if c.is_locked {
            tags.push("OPEN IN LIGHTROOM".to_string());
        }
        if let Some(n) = c.image_count {
            tags.push(pc_core::count_en(n, "file", "files"));
        }
        if let Some(e) = &c.read_error {
            tags.push(format!("read error: {e}"));
        }
        let suffix = if tags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", tags.join(", "))
        };
        println!("{}{}", c.path, suffix);
    }
    Ok(())
}
