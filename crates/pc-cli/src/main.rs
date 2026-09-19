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
    about = "Разбор фотоархива: дубликаты, серии, регенерируемые данные"
)]
struct Cli {
    /// Путь к базе. По умолчанию ./photo-cleanup.db
    #[arg(long, global = true, default_value = "photo-cleanup.db")]
    db: PathBuf,

    #[arg(long, global = true, default_value = "info")]
    log: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Обойти корни и составить опись производных данных
    Scan(ScanArgs),
    /// Прочитать изображения и построить индекс
    Index(IndexArgs),
    /// Регенерируемые данные: превью Lightroom, кэши, системный мусор
    #[command(subcommand)]
    Derived(DerivedCmd),
    /// Семейства: один кадр — несколько представлений
    #[command(subcommand)]
    Families(FamiliesCmd),
    /// Сводка по базе
    Status,
    /// Найденные каталоги Lightroom
    Catalogs,
}

#[derive(Args)]
struct ScanArgs {
    /// Корень обхода. Указывать /mnt/diskN/..., не /mnt/user/...
    #[arg(long = "root", required = true)]
    roots: Vec<PathBuf>,
}

#[derive(Args)]
struct IndexArgs {
    #[arg(long = "root", required = true)]
    roots: Vec<PathBuf>,
    /// Каталог кэша тамбнейлов. По умолчанию рядом с базой.
    #[arg(long)]
    thumbs: Option<PathBuf>,
    /// Минимальный размер файла; меньше — иконки и ассеты, не фотографии
    #[arg(long, default_value = "100K", value_parser = format::parse_size)]
    min_size: i64,
    /// Читателей на физический диск. Больше двух на HDD только вредит.
    #[arg(long, default_value_t = 2)]
    readers_per_disk: usize,
    /// Перечитать даже то, что уже в индексе
    #[arg(long)]
    reindex: bool,
}

#[derive(Subcommand)]
enum FamiliesCmd {
    /// Построить семейства по связям и по сходству
    Build(BuildArgs),
    /// Показать семейства
    List(FamListArgs),
}

#[derive(Args)]
struct BuildArgs {
    #[arg(long)]
    thumbs: Option<PathBuf>,
    /// Порог расстояния pHash для кандидатов
    #[arg(long, default_value_t = 10)]
    phash_max: u32,
    /// Минимальный SSIM, при котором пара считается одним кадром
    #[arg(long, default_value_t = 0.90)]
    ssim_min: f64,
}

#[derive(Args)]
struct FamListArgs {
    #[arg(long, default_value_t = 20)]
    limit: i64,
    #[arg(long, default_value_t = 0)]
    offset: i64,
    /// Показывать и одиночные кадры
    #[arg(long)]
    all: bool,
    /// Пути, связи и разбор оценки
    #[arg(long, short)]
    verbose: bool,
}

#[derive(Subcommand)]
enum DerivedCmd {
    /// Показать найденные бандлы
    List(ListArgs),
    /// Перенести в карантин
    Clean(CleanArgs),
    /// Удалить из карантина навсегда
    Purge(PurgeArgs),
    /// Вернуть из карантина по записи журнала
    Undo(UndoArgs),
}

#[derive(Args)]
struct ListArgs {
    #[arg(long)]
    kind: Option<String>,
    /// Минимальный размер, например 100M
    #[arg(long, value_parser = format::parse_size)]
    min_size: Option<i64>,
    /// Показывать и заблокированные, и уже перенесённые
    #[arg(long)]
    all: bool,
}

#[derive(Args)]
struct CleanArgs {
    /// Вид данных; можно повторять. По умолчанию lr-previews
    #[arg(long = "kind")]
    kinds: Vec<String>,
    #[arg(long, value_parser = format::parse_size)]
    min_size: Option<i64>,
    /// Только показать, что будет сделано
    #[arg(long)]
    dry_run: bool,
    /// Подтверждение выполнения
    #[arg(long)]
    yes: bool,
    /// Куда складывать карантин. По умолчанию <точка монтирования>/.photo-cleanup-quarantine.
    /// Путь обязан быть на той же файловой системе, что и данные.
    #[arg(long)]
    quarantine: Option<PathBuf>,
}

#[derive(Args)]
struct PurgeArgs {
    /// Удержание, например 7d
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
            println!("Запись {} откачена.", a.journal);
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
                println!("Нечего показать. Сначала `families build`.");
                return Ok(());
            }
            for f in &fams {
                pc_cli::families::print_family(f, a.verbose);
            }
            pc_cli::families::print_summary(&db)?;
            Ok(())
        }
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
        println!("Ничего не найдено. Сначала выполните `photo-cleanup scan --root ...`.");
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
                "вид «{}» ({}) не подлежит удалению: регенерации нет",
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
        println!("Под условия ничего не подходит.");
        return Ok(());
    }

    let files: i64 = selected.iter().map(|b| b.file_count).sum();
    let bytes: i64 = selected.iter().map(|b| b.size).sum();
    println!(
        "К переносу в карантин: {}, {}, {}\n",
        pc_core::count_ru(selected.len() as i64, "бандл", "бандла", "бандлов"),
        pc_core::count_ru(files, "файл", "файла", "файлов"),
        fmt_bytes(bytes as u64)
    );
    format::print_grouped_opts(&selected, false);

    if a.dry_run {
        println!("\n--dry-run: ничего не изменено.");
        return Ok(());
    }
    if !a.yes {
        println!("\nДля выполнения добавьте --yes.");
        return Ok(());
    }

    let run_id = db
        .latest_run()?
        .context("нет ни одного прогона, сначала выполните scan")?;
    let totals = pc_apply::quarantine_many(db, run_id, &selected, a.quarantine.as_deref())?;

    println!("\nПеренесено: {}", totals.summary());
    for s in &totals.skipped {
        println!("  пропущено: {s}");
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
        println!("Нечего удалять: в карантине нет записей старше указанного срока.");
        return Ok(());
    }
    let files: i64 = pending.iter().map(|e| e.file_count).sum();
    let bytes: i64 = pending.iter().map(|e| e.size).sum();

    println!(
        "Будет удалено безвозвратно: {}, {}, {}",
        pc_core::count_ru(pending.len() as i64, "объект", "объекта", "объектов"),
        pc_core::count_ru(files, "файл", "файла", "файлов"),
        fmt_bytes(bytes as u64)
    );
    for e in pending.iter().take(10) {
        println!("  {}", e.dst.as_deref().unwrap_or(&e.src));
    }
    if pending.len() > 10 {
        println!("  … и ещё {}", pending.len() - 10);
    }

    if !a.yes {
        println!("\nЭто необратимо. Для выполнения добавьте --yes.");
        return Ok(());
    }
    let totals = pc_apply::purge(db, a.older_than)?;
    println!("\nУдалено: {}", totals.summary());
    for s in &totals.skipped {
        println!("  пропущено: {s}");
    }
    Ok(())
}

fn print_index_breakdown(db: &Db) -> Result<()> {
    let rows = db.container_counts()?;
    if rows.is_empty() {
        return Ok(());
    }
    println!("\nПо контейнерам:");
    for (name, count, bytes) in rows {
        println!(
            "  {:<16} {:>8}  {:>10}",
            name,
            pc_core::count_ru(count, "файл", "файла", "файлов"),
            fmt_bytes(bytes as u64)
        );
    }
    let lied = db.mislabelled_count()?;
    if lied > 0 {
        println!(
            "\n{} — расширение не совпало с содержимым.",
            pc_core::count_ru(lied, "файл", "файла", "файлов")
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

    println!("Бандлов в описи:     {}", all.len());
    println!(
        "  можно перенести:   {:>4}  {}",
        removable.len(),
        fmt_bytes(sum(&removable))
    );
    println!(
        "  заблокировано:     {:>4}  {}",
        blocked.len(),
        fmt_bytes(sum(&blocked))
    );

    let idx = db.index_stats()?;
    if idx.total > 0 {
        println!(
            "\nВ индексе файлов:    {}\n  изображений:       {:>4}\n  пропущено:         {:>4}",
            idx.total, idx.images, idx.skipped
        );
    }

    let q = pc_apply::quarantined_totals(db)?;
    println!("\nВ карантине:         {}", q.summary());
    let pend = db.journal_pending()?;
    if !pend.is_empty() {
        println!(
            "\nВНИМАНИЕ: {} незавершённых записей журнала — прогон был прерван.",
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
        println!("Каталоги Lightroom не найдены.");
        return Ok(());
    }
    for c in &cats {
        let mut tags = Vec::new();
        if c.is_backup {
            tags.push("бэкап".to_string());
        }
        if c.is_locked {
            tags.push("ОТКРЫТ В LIGHTROOM".to_string());
        }
        if let Some(n) = c.image_count {
            tags.push(pc_core::count_ru(n, "файл", "файла", "файлов"));
        }
        if let Some(e) = &c.read_error {
            tags.push(format!("ошибка чтения: {e}"));
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
