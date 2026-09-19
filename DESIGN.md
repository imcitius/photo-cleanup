# photo-cleanup — проектный документ

Статус: проектирование. Реализации нет.
Дата замеров архива: 2026-09-19.

---

## 1. Задача

Разобрать фотоархив, накопленный за много лет: найти дубликаты (включая копии в
другом формате и разрешении), выбрать лучший вариант, безопасно убрать остальное.
Дополнительно: разбор серий с выбором лучшего кадра, категоризация (сканы,
документы, счётчики, скриншоты), опциональная реорганизация структуры папок.

Что инструмент **не** делает:
- не удаляет файлы (только перенос в карантин + журнал отката);
- не распознаёт лица (детекция без идентификации — да, см. §9.3);
- не трогает видео в v1 (см. §14);
- не пишет внутрь библиотек Apple Photos.

---

## 2. Фактическое состояние архива

Целевое дерево: `/mnt/disk{1,2,3}/data/media/foto/`, кроме `Video/`.

### 2.1 Изображения — цель работы

| формат | файлов | GiB |
|---|---:|---:|
| jpg | 39 384 | 116.7 |
| arw (Sony RAW) | 4 290 | 88.8 |
| dng | 4 955 | 59.5 |
| tif/tiff | 1 473 | 16.2 |
| bmp | 140 | 3.8 |
| cr2 | 102 | 0.7 |
| png/psd/webp/xcf | 54 | 0.2 |
| **итого** | **50 398** | **286** |

Плюс неопределённое число фотографий без расширения (см. 2.3).

### 2.2 Прочее содержимое `foto/`

- **Видео россыпью внутри D/E/F: 366 GiB** (mp4 188, avi 104, mov 50, mpg 16,
  wmv 4, vob 3, mts 1). Отдельно папка `Video/` — 389 GiB.
- **Регенерируемые превью Lightroom: ≥92 GiB** — 24 520 JPEG без расширения
  внутри `*.lrdata`, плюс `.lrprev` 10 755 файлов / 2.9 GiB.
- Каталоги Lightroom: 18 шт., `.lrcat` суммарно 0.2 GiB.
- Архивы: 4 zip на 40 MiB, все — бэкапы `.lrcat`. Работа с архивами не нужна.

### 2.3 Открытый вопрос

~11 800 файлов без расширения (~81 GiB, средний размер 7 MiB) не попали в
учтённые `.lrdata`. Это либо превью более глубоко вложенных библиотек в `D/`
и `E/`, либо настоящие фотографии, потерявшие расширение. На архитектуру не
влияет — покрыто определением типа по magic bytes.

### 2.4 Особенности источника

- `D/`, `E/`, `F/` — дампы старых винчестеров. Фото перемешаны с системным
  мусором (dll, so, базы, бэкапы). Нужны фильтры сверх скоупа по каталогам.
- `Lightroom_lib/` — живой рабочий каталог (изменён недавно), в отличие от
  архивных дампов. Даёт приоритет путей по умолчанию.
- По всему массиву 122 417 JPG, внутри `foto/` — 39 384. Скоуп по каталогам
  отсекает 83 тысячи иконок и ассетов бесплатно.
- В архиве **нет HEIC**. libheif — за feature-флагом, не в v1.
- В корне есть `.DS_Store` → будут AppleDouble `._*`.

### 2.5 Железо

- Unraid, Ryzen 5 7600X (6c/12t, Zen 4, AVX-512 + VNNI), 16 GB RAM, Docker.
- disk1 WD40EFPX 3.7T (2.5T free), disk2 WD40EFPX 3.7T (2.7T free),
  disk3 Toshiba MG08 7.3T (4.3T free). Фото — преимущественно на disk2 и disk3.
- Кэш NVMe 477G, свободно 101G.
- **Паритета нет.** Оба слота `DISK_NP_DSBL`, `sbSynced=0`. Массив без
  избыточности — см. §12.1.
- Mac M3 Pro 36 GB есть, но для этой задачи не нужен (§3.2).

---

## 3. Ключевые решения

### 3.1 Всё считается на Unraid, локально

7600X — десктопный Zen 4, не слабый NAS-процессор. При корпусе в 50 тысяч
изображений полный проход занимает ~15 минут (§11). Сеть 2.5 GbE не
задействуется: инструмент работает в Docker на самой машине.

### 3.2 Apple Neural Engine не используется

Задача упирается в IO и JPEG-декод, а не в матричные умножения. Единственная
стадия, где помог бы ANE — эмбеддинги — на 7600X с INT8/VNNI занимает ~6 минут.
Разделение конвейера между NAS и Mac не окупается.

Путь на Mac сохраняется как опция: thumbnail-кэш (~1.5 GiB) переносится куда
угодно, инференс идёт через ONNX Runtime с выбором execution provider
(`cpu` / `coreml` / `cuda` / `directml`), модель одна и та же.

### 3.3 Никаких ANN-индексов — точный полный перебор

На 75 тысячах векторов приближённый поиск медленнее и сложнее точного.

- **Эмбеддинги.** Матрица 75k × 512 f32 = 154 MB. Все попарные косинусы — один
  GEMM: 5.8 TFLOP. Zen4 на 6 ядрах даёт ~670 GFLOPS на GEMM → **~9 секунд**.
  Блочно, полную матрицу не материализуем, храним top-K по строке.
- **pHash.** 2.8 млрд пар, XOR + `VPOPCNTDQ` → **меньше секунды**.

Убирает из проекта `usearch`/`hnsw_rs`, BK-дерево, их персистентность и
инвалидацию. Результат при этом точный, а не приближённый.

### 3.4 Тип файла — по magic bytes, никогда по расширению

24 520 файлов без расширения оказались JPEG. Обратное тоже встречается.
Расширение — подсказка для приоритета чтения, не более.

### 3.5 RAW читаем через встроенное превью

ARW/DNG/CR2 — контейнеры с индексом (TIFF/IFD). Полноразмерный или
крупный JPEG-превью извлекается без декодирования сенсорных данных.
149 GiB RAW → ~12 GiB фактического чтения.

Механика: первые 64 KiB дают заголовок и смещение превью, затем один seek.
Работу внутри диска сортируем по inode, чтобы головка шла вперёд.

JPEG так нельзя — энтропийный поток последовательный, файл читается целиком;
экономим только CPU через `scale_denom` библиотеки libjpeg-turbo.

### 3.6 Язык: Rust

Производительность больше не аргумент — корпус мал. Оправдание:
один статический бинарь под Docker, прямой FFI к libjpeg-turbo и libraw,
отсутствие GC на долгих проходах, задел на инкрементальный режим и видео
(755 GiB, там счёт другой).

---

## 4. Модель данных

### 4.1 Кадр, представление, роль

Единица смысла — **один спуск затвора** (`family`). У него несколько
**представлений** — файлов, каждый со своей **ролью**:

| роль | что это | удаляется по умолчанию |
|---|---|---|
| `original` | RAW или самый ранний/крупный член | нет |
| `camera-jpg` | JPEG из камеры рядом с RAW | нет |
| `converted` | DNG, сконвертированный из RAW | нет |
| `export` | экспорт из Lightroom/Photoshop | нет |
| `resize` | уменьшенная производная | по порогу разрешения |
| `copy` | побайтово или попиксельно идентичный близнец | **да** |
| `unknown` | роль не определена | нет, ручной разбор |

Дубликатом по умолчанию считается только `copy`. Всё остальное —
представления, они сохраняются, пока пользователь не решит иначе.

### 4.2 Провенанс берётся из метаданных, а не угадывается

| связь | источник | точность |
|---|---|---|
| DNG ← RAW | тег DNG `OriginalRawFileName` | точная |
| экспорт ← оригинал | XMP `xmpMM:DerivedFrom`, `DocumentID`, `OriginalDocumentID` | точная |
| один спуск затвора | `DateTimeOriginal` + `SerialNumber` тушки + `SonyImageNumber`/`ShutterCount` | точная |
| RAW ↔ камерный JPEG | общий basename + общий `DateTimeOriginal` + нетронутый EXIF камеры | точная |
| virtual copies, стеки | каталог Lightroom | точная |
| остальное | pHash → SSIM → ORB+RANSAC | эвристика |

Перцептивное сравнение работает только там, где точных связей нет.

### 4.3 Тиры совпадения

| тир | что | сигнал |
|---|---|---|
| T0 | байт-в-байт | blake3 |
| T1 | пиксели совпадают, отличается контейнер/EXIF | хеш декодированного RGB |
| T2 | тот же кадр, другое разрешение/качество | pHash → **SSIM обязательно** |
| T3 | кроп, поворот, редакт | эмбеддинг → **ORB+RANSAC обязательно** |
| T4 | та же сцена, другой кадр | эмбеддинг, низкий порог |

T4 — **не дубликат**, это серия (§9). Смешение T2 и T4 уничтожило бы серийную
съёмку.

Правило: из кандидата в T2 или T3 продвигаем только после геометрической
верификации. pHash и эмбеддинг — фильтры, не доказательства.

---

## 5. Конвейер

```
0  walk        обход /mnt/diskN напрямую, шардинг по физическим дискам,
               1-2 читателя на диск, сортировка по inode
               фильтры: magic bytes, размер >100 KiB, мин. сторона >512 px,
               стоп-лист путей
1  extract     RAW/контейнеры → seek к превью; JPEG → полное чтение
               EXIF, XMP, magic, размеры, ориентация
               blake3 файла, thumbnail 384px → blob-стор
2  lr-index    чтение .lrcat (read-only копия), слияние 18 каталогов
3  hash        pixel-hash после нормализации ориентации,
               pHash/dHash + pHash по центру и 4 квадрантам
4  embed       MobileCLIP-S0 INT8 через ONNX Runtime по thumbnail
5  provenance  точные связи (§4.2) → предварительные семейства и роли
6  match       брутфорс GEMM + popcount → кандидаты
               SSIM-верификация для T2, ORB+RANSAC для T3
               union-find → достройка семейств
7  series      группировка серий, скоринг кадров
8  classify    zero-shot категории
9  quality     скоринг keeper внутри семейства
10 plan        правила → план действий, счётчики
11 apply       перенос в карантин, верификация, журнал
```

Стадии 0-4 идемпотентны по ключу `(path, size, mtime, inode)` и
чекпоинтятся — многочасовой прогон можно прервать и продолжить.
Стадии 5-10 работают только по SSD-кэшу, перезапускаются за секунды с
любыми порогами.

---

## 6. Схема БД

SQLite + WAL на NVMe-кэше. Тамбнейлы — отдельный content-addressed стор
`thumbs/ab/cd/<blake3>.jpg` (≈1.5 GiB), не в базе: проще переносить.

```sql
CREATE TABLE runs(
  id INTEGER PRIMARY KEY, started_at INTEGER, finished_at INTEGER,
  roots TEXT, tool_version TEXT);

CREATE TABLE files(
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,
  disk TEXT NOT NULL,              -- disk1/2/3: шардинг и per-disk карантин
  dev INTEGER, inode INTEGER, nlink INTEGER,
  size INTEGER NOT NULL, mtime INTEGER NOT NULL,
  kind TEXT,                       -- image/video/sidecar/catalog/derived/other
  format TEXT,                     -- по magic bytes
  width INTEGER, height INTEGER, bit_depth INTEGER, orientation INTEGER,
  blake3 BLOB, pixel_hash BLOB,
  phash INTEGER, dhash INTEGER, phash_crops BLOB,   -- 5 × u64
  thumb_key BLOB,
  excluded_reason TEXT,            -- lrdata/system/too-small/appledouble/...
  first_seen_run INTEGER, last_seen_run INTEGER);

CREATE TABLE meta(
  file_id INTEGER PRIMARY KEY REFERENCES files(id),
  taken_at INTEGER, taken_at_source TEXT,   -- exif/filename/path/mtime
  camera_make TEXT, camera_model TEXT, body_serial TEXT, lens TEXT,
  iso INTEGER, shutter TEXT, aperture REAL, focal REAL,
  shot_seq INTEGER,                -- SonyImageNumber / ShutterCount
  burst_id TEXT,                   -- BurstUUID / ContentIdentifier
  gps_lat REAL, gps_lon REAL,
  software TEXT,
  xmp_document_id TEXT, xmp_original_document_id TEXT, xmp_derived_from TEXT,
  dng_original_raw TEXT,
  jpeg_quality INTEGER);           -- оценка по таблицам квантования

CREATE TABLE lr_catalogs(
  id INTEGER PRIMARY KEY, path TEXT, name TEXT,
  schema_version TEXT, indexed_at INTEGER, is_backup INTEGER);

CREATE TABLE lr_refs(
  catalog_id INTEGER REFERENCES lr_catalogs(id),
  file_id INTEGER REFERENCES files(id),
  rating INTEGER, color_label TEXT, pick_flag INTEGER,
  has_develop_edits INTEGER, is_virtual_copy INTEGER, stack_id TEXT,
  collections TEXT,
  PRIMARY KEY(catalog_id, file_id));

CREATE TABLE embeddings(
  file_id INTEGER PRIMARY KEY REFERENCES files(id),
  model TEXT, vec BLOB);

CREATE TABLE families(
  id INTEGER PRIMARY KEY,
  key_kind TEXT,      -- shutter_id / xmp_chain / dng_link / perceptual
  confidence REAL, taken_at INTEGER, camera TEXT,
  representative_file INTEGER REFERENCES files(id));

CREATE TABLE family_members(
  family_id INTEGER REFERENCES families(id),
  file_id INTEGER REFERENCES files(id),
  role TEXT, parent_file INTEGER REFERENCES files(id),
  role_evidence TEXT,          -- json: на чём основана роль
  tier TEXT,                   -- T0..T4 относительно родителя
  quality REAL, quality_breakdown TEXT,
  PRIMARY KEY(family_id, file_id));

CREATE TABLE series(
  id INTEGER PRIMARY KEY,
  kind TEXT,                   -- burst / bracket / pixel-shift / timelapse
  started_at INTEGER, camera TEXT,
  best_family INTEGER REFERENCES families(id),
  protected INTEGER);          -- pixel-shift: запрет на удаление внутри

CREATE TABLE series_members(
  series_id INTEGER REFERENCES series(id),
  family_id INTEGER REFERENCES families(id),
  sharpness REAL, exposure REAL, eyes_open REAL, aesthetic REAL,
  rank INTEGER, locked INTEGER,
  PRIMARY KEY(series_id, family_id));

CREATE TABLE categories(
  id INTEGER PRIMARY KEY, name TEXT, prompt TEXT,
  threshold REAL, enabled INTEGER);

CREATE TABLE file_categories(
  file_id INTEGER REFERENCES files(id),
  category_id INTEGER REFERENCES categories(id),
  score REAL, PRIMARY KEY(file_id, category_id));

CREATE TABLE derived_bundles(
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,
  disk TEXT NOT NULL,
  kind TEXT NOT NULL,          -- lr-previews / lr-smart-previews / lr-helper /
                               -- co-cache / on1-cache / thumbs-db / appledouble / ...
  owner_ref TEXT,              -- путь к .lrcat, если владелец определён
  file_count INTEGER, size INTEGER, newest_mtime INTEGER,
  regenerable INTEGER,         -- 0 = удалять запрещено
  blocked_reason TEXT,         -- catalog-open / originals-missing / not-regenerable
  rebuild_cost_hint TEXT,
  scanned_run INTEGER);

CREATE TABLE policy(
  id INTEGER PRIMARY KEY, kind TEXT, expr TEXT, weight REAL, enabled INTEGER);

CREATE TABLE plan(
  id INTEGER PRIMARY KEY, file_id INTEGER REFERENCES files(id),
  op TEXT,                     -- keep / quarantine / recategorize / reorganize
  dest TEXT, reason TEXT,
  decided_by TEXT,             -- policy / user
  decided_at INTEGER);

CREATE TABLE journal(
  id INTEGER PRIMARY KEY, plan_id INTEGER REFERENCES plan(id),
  op TEXT, src TEXT, dst TEXT, size INTEGER, blake3 BLOB,
  applied_at INTEGER, verified INTEGER, undone_at INTEGER);
```

В реализации таблицы `plan` нет: план дедупа и план раскладки считаются
заново из ролей и дат каждый раз — хранить решение, которое устарело вместе
с индексом, опаснее, чем пересчитать. Журнал остаётся единственной записью
о том, что было сделано, с операциями `quarantine`, `quarantine-file` и
`organize`; у последней `src` и `dst` — это и есть маппинг путей раскладки.

Индексы: `files(blake3)`, `files(pixel_hash)`, `files(size)`,
`files(dev,inode)`, `files(disk)`, `meta(taken_at)`,
`meta(body_serial, shot_seq)`, `meta(dng_original_raw)`,
`meta(xmp_document_id)`, `family_members(file_id)`.

---

## 7. Границы модулей

```
pc-core        типы, определение диска и точки монтирования, кэш тамбнейлов
pc-walk        обход, шардинг по дискам, фильтры, стоп-листы, бандлы
pc-image       magic-sniff, EXIF/XMP, разбор TIFF/IFD, превью, тамбнейлы
pc-hash        blake3, pixel-hash, pHash/dHash, crop-hashes, SSIM
pc-family      точные связи, перцептивные кандидаты, роли, скоринг keeper
pc-lightroom   чтение .lrcat только на чтение, проверка наличия мастеров
pc-organize    дата по лестнице источников, события, план раскладки
pc-apply       карантин, откат, purge, журнал, переносы раскладки
pc-db          SQLite, миграции, запросы
pc-api         axum + встроенный фронтенд
pc-cli         команды

Ещё не написан: pc-embed (ONNX, эмбеддинги и категоризатор).

Серии, ранжирование кадров, виды и политика с планом вошли в pc-family
(модули `series`, `categories`, `plan`), метрики качества — в pc-image
(`metrics`).

Категоризация разделилась надвое против исходного плана: признаки, которые
измеряются по пикселям (сканы, документы, скриншоты, пустые кадры), не
требуют модели и уже работают; смысловые виды («фото счётчика») требуют
эмбеддингов и ждут pc-embed.

Отклонения от исходного плана, принятые при реализации:

* **libraw и mozjpeg-sys не нужны.** Разбор IFD на чистом Rust достаёт
  встроенное превью из любого TIFF-контейнера, а `scale_denom` не окупается
  при корпусе такого размера.
* **pc-provenance, pc-match и pc-quality объединены в pc-family** — они
  работают над одними данными и делят union-find.
* **pc-decode переименован в pc-image.**
```

CLI:

```
photo-cleanup scan    --root /mnt/disk2/data/media/foto --root /mnt/disk3/... \
                      --db /appdata/pc.db --exclude-from excludes.txt
photo-cleanup lr-index
photo-cleanup derived list  [--kind lr-previews]
photo-cleanup derived clean --kind lr-previews [--min-size 100M] [--dry-run]
photo-cleanup derived purge --older-than 7d
photo-cleanup embed   [--ep cpu|coreml|cuda]
photo-cleanup match
photo-cleanup plan    --policy policy.toml
photo-cleanup apply   --quarantine per-disk
photo-cleanup organize plan  --root /mnt/disk3/Photo [--gap 6h] [--skip-uncertain]
photo-cleanup organize apply --root /mnt/disk3/Photo --yes
photo-cleanup organize undo  [--run <id>] --yes
photo-cleanup organize runs
photo-cleanup undo    --journal <id>
photo-cleanup serve   --bind 0.0.0.0:8080
```

---

## 8. HTTP API

```
GET  /api/status                    прогресс, счётчики
GET  /api/events                    SSE: прогресс сканирования
GET  /api/families?role=&tier=&sort=&cursor=
GET  /api/families/:id
POST /api/families/:id/keeper       {file_id}
POST /api/families/:id/split        ложное объединение
GET  /api/series?kind=&cursor=
POST /api/series/:id/best           {family_id}
GET  /api/files/:id/thumb?size=384
GET  /api/files/:id/full            стрим оригинала для pixel-peep
GET  /api/policy | PUT /api/policy
GET  /api/plan/summary              файлов и GiB под текущей политикой
POST /api/plan/apply                -> job id
POST /api/plan/undo                 {journal_id}
GET  /api/categories | POST /api/categories   {name, prompt, threshold}
GET  /api/organize?root=&gap_hours=  предпросмотр раскладки: события,
                                     источники дат, отказы, команда
GET  /api/derived                   бандлы регенерируемых данных
GET  /api/derived/:id
POST /api/derived/clean             {ids:[...]} -> job id
POST /api/derived/purge             {older_than_days}
```

---

## 9. Интерфейс

Веб. Один бинарь со встроенной статикой, JSON API, SSE для прогресса.
Позже при желании оборачивается в Tauri — фронт тот же.

### 9.1 Экран семейства — дерево, а не сетка

```
📷  DSC01234 · 14.07.2019 18:32:05 · A7 III · 24 МП        6 файлов, 61 МБ
│
├── ORIGINAL    DSC01234.ARW       24.1 MB  6000×4000  ★★★★☆  LR: Main library
│   └── sidecar DSC01234.xmp
├── CAMERA JPG  DSC01234.JPG        8.2 MB  6000×4000         из камеры
├── CONVERTED   DSC01234.dng       22.8 MB  6000×4000         OriginalRawFileName
├── EXPORT      DSC01234-Edit.jpg   4.1 MB  6000×4000         xmpMM:DerivedFrom
│   ├── COPY    DSC01234-Edit.jpg   4.1 MB  идентичен         /Backup/foto/2019/
│   └── RESIZE  DSC01234-Edit.jpg    380 KB 1200×800          /Web/
└── RESIZE      IMG_20190714.jpg     180 KB 1080×720          /Telegram/
```

Отступы показывают происхождение. За секунду видно, что ARW, JPG и DNG —
три разных объекта одного кадра, а не три копии.

### 9.2 Политика по ролям — одно решение вместо тысяч

```
Удалять:  COPY [x]   RESIZE <2 МП [x]   EXPORT [ ]   CAMERA-JPG [ ]   CONVERTED [ ]
→ 19 332 файла, 214 ГиБ
```

Счётчик пересчитывается мгновенно (всё на SSD). Руками разбирается только
то, где провенанс не определился.

Дефолт по решению пользователя: RAW и камерный JPEG держим оба,
экспорты храним отдельно и не удаляем.

### 9.3 Серии и лучший кадр

Группировка: `DateTimeOriginal` ± 3-10 с, та же камера, `BurstUUID`,
подтверждение эмбеддингом.

Скоринг:
- резкость — вариация лапласиана по области с верхним квантилем градиента,
  не по всему кадру (боке ломает глобальную метрику);
- различение смаза и расфокуса по анизотропии направленных градиентов;
- экспозиция — доля клиппинга в тенях и светах, энтропия гистограммы;
- эстетика — NIMA поверх уже посчитанного эмбеддинга (~50 MFLOP);
- **открытость глаз** — детекция лица (YuNet, ~1 мс) + eye aspect ratio по
  5 точкам. Идентификация не выполняется, эмбеддинги лиц не считаются и не
  хранятся. Отдельный флаг. Без этого моргнувший кадр не отличить.

### 9.4 Категоризатор — zero-shot, без разметки

Косинус эмбеддинга изображения к эмбеддингам текстовых промптов. Текстовая
часть считается один раз, классификация 50k файлов — секунды. Новая
категория добавляется вводом фразы, переиндексация не нужна.

Стартовый набор: сканы документов, счётчики, чеки, скриншоты, доски,
удостоверения, мемы с текстом, люди, пейзажи.

Для документов дополнительно дешёвый не-NN признак: доля текстовых регионов
(MSER / stroke width), почти бинарная гистограмма, соотношение сторон A4 —
часто точнее CLIP на сканах.

Действие для категорий — перенос, не удаление. Отдельная фаза от дедупа.

### 9.5 Остальные экраны

- **Дашборд**: возвращаемое место по тирам и ролям, разбивка по форматам, прогресс.
- **Регенерируемые данные**: превью Lightroom и кэши — победа без риска (§10).
- **Сравнение**: два кадра, синхронный зум, diff.
- **Правила**: приоритеты путей и форматов.
- **План и применение**: dry-run, дельта по месту, выполнение, откат.

Клавиатура обязательна везде: J/K навигация, Enter принять, Space сменить
keeper, X пропустить. Тысячи семейств мышкой не разобрать.

---

## 10. Очистка регенерируемых данных

Самостоятельная функция, не зависящая от дедупа. Не требует декодирования,
эмбеддингов и сравнения — только обход, сопоставление с правилами, перенос и
журнал. Даёт ≥92 GiB на текущем архиве.

### 10.1 Классификация

| вид | путь | регенерируемо | примечание |
|---|---|---|---|
| Превью Lightroom | `<Catalog> Previews.lrdata/` | да | основной объём; LR пересоберёт при просмотре |
| Smart Previews | `<Catalog> Smart Previews.lrdata/` | **условно** | см. 10.2 — нужны, когда оригиналы недоступны |
| Helper-данные LR | `<Catalog> Helper.lrdata/` | да | у нас 0.2–8 MB на каталог, овчинка не стоит выделки — не трогаем |
| **Данные каталога LR** | `<Catalog>.lrcat-data/` | **нет** | ИИ-маски, Denoise, часть данных обработки. **Удаление необратимо теряет маски.** Жёсткий запрет |
| Журнал/лок LR | `*.lrcat-journal`, `*.lrcat.lock` | — | транзиентное; наличие `.lock` означает открытый каталог |
| Бэкапы каталогов | `Backups/*/*.lrcat.zip`, `Old Lightroom Catalogs/` | — | не кэш, а пользовательские бэкапы; отдельная политика с удержанием N свежих |
| Capture One | `Cache/`, `Proxies/` внутри сессии | да | в архиве не обнаружены |
| ON1 | кэш-каталоги | да | папка `ON1/` пуста; `.on1`-сайдкары — **не кэш**, содержат правки |
| Apple Photos | `resources/derivatives/` | — | внутрь бандла не пишем никогда |
| Системный мусор | `Thumbs.db`, `.DS_Store`, `._*`, `@eaDir`, `.thumbnails` | да | мелочь по объёму, но чистит листинги |

### 10.2 Гейт для Smart Previews

Smart Previews — это lossy-DNG прокси, позволяющие редактировать при
отключённом диске с оригиналами. Пересоздаются только если оригиналы на месте.

Правило: удаление предлагается **лишь когда все оригиналы, на которые ссылается
каталог, найдены на диске**. Проверка — сверка `lr_refs` против `files`.
Хотя бы один недостающий оригинал → бандл помечается
`blocked_reason = originals-missing` и в план не попадает.

### 10.3 Правила безопасности

1. **Открытый каталог не трогаем.** Наличие `<Catalog>.lrcat.lock` означает,
   что Lightroom работает с ним прямо сейчас. Удаление превью на ходу может
   повредить каталог. Бандл блокируется с `blocked_reason = catalog-open`.
2. **`.lrcat-data` заблокирован на уровне кода**, а не политики. Его нельзя
   выбрать в интерфейсе и нельзя указать в `--kind`.
3. **Единица действия — бандл целиком**, а не отдельные файлы внутри.
   Переносим `X Previews.lrdata/` одним объектом.
4. **Карантин, а не удаление** — как и везде. На том же диске, поэтому
   перенос 92 GiB это серия `rename(2)`, мгновенно.
   **Место вернётся только после `purge`** — интерфейс говорит об этом прямо.
5. **Удержание по умолчанию 7 дней**, затем `purge` по явной команде.
6. Сверка перед переносом: бандл не изменился с момента сканирования
   (`newest_mtime`, `file_count`).

### 10.4 Стоимость пересборки

Предупреждение показывается по каждому каталогу: Lightroom перестроит
стандартные превью при первом просмотре папки. Порядок величины —
десяток-другой минут на пару тысяч кадров, зависит от размера превью в
настройках каталога. Данные при этом не теряются, теряется только время.

Оценка `rebuild_cost_hint` считается из числа снимков в каталоге.

### 10.5 Обход

Каталоги-бандлы определяются по имени и **не раскрываются** (`prune`).
Вместо 24 520 строк в `files` появляется одна строка в `derived_bundles`
с агрегатами. Это ускоряет и сам обход, и все последующие стадии.

### 10.6 Интерфейс

Таблица, сгруппированная по видам:

```
ПРЕВЬЮ LIGHTROOM                                          вернётся 91.4 GiB
[x] Dogshow_15.02.2025 Previews.lrdata      2 582 файла   2.3 GiB   ~15 мин
[x] DogShow_Holon_13-14.02.2026 Previews    2 112 файлов  1.6 GiB   ~12 мин
[ ] Work Smart Previews.lrdata                 —          197 MiB   ЗАБЛОКИРОВАНО
    оригиналы не найдены: 412 из 8 903
[—] Main library.lrcat-data                    —          —         НЕ УДАЛЯЕТСЯ
    ИИ-маски и Denoise, регенерации нет

СИСТЕМНЫЙ МУСОР                                            вернётся 0.3 GiB
[x] .DS_Store, ._*, Thumbs.db          1 843 файла         0.3 GiB
```

Выбор целиком по виду одной кнопкой, бегущий итог сверху, заблокированные
показаны с причиной и не выбираются.

### 10.7 Команды

```
photo-cleanup derived list  [--kind lr-previews]
photo-cleanup derived clean --kind lr-previews [--min-size 100M] [--dry-run]
photo-cleanup derived purge --older-than 7d
```

### 10.8 Почему это делается первым

Функция независима от всего остального конвейера и при этом прогоняет весь
рискованный слой сантехники — обход с шардингом по дискам, per-disk карантин,
журнал, откат, верификацию — на данных, где цена ошибки равна времени
пересборки превью, а не потере фотографии.

## 11. Бюджет производительности

| стадия | время |
|---|---|
| walk + stat | секунды |
| чтение: 137 GiB обычных + RAW через превью (149 GiB → ~12 GiB) | ~7 мин |
| декод + хеши, 12 потоков | ~1 мин, перекрывается с IO |
| MobileCLIP-S0 INT8, 50k файлов | ~6 мин |
| попарное сравнение (GEMM + popcount) | ~10 с |
| SSIM + ORB на кандидатах | ~2 мин |
| **полный проход** | **~15 мин** |

Память: целевой потолок 4-6 GiB, задаётся флагом. Эмбеддинги 154 MB,
буферы scaled-декода ~0.5 MB на поток. Единственный риск — полный декод RAW
(150-250 MB на кадр), поэтому RAW идёт через превью; файлы без превью — в
отдельную очередь с лимитом 1-2 параллельных декода.

---

## 12. Безопасность

### 11.1 Массив без избыточности

Паритета нет, отказ любого диска означает потерю данных. Перед фазой apply
нужно либо добавить паритетный диск, либо скопировать `foto/` вовне.
Карантин защищает от ошибок инструмента, но не от смерти диска.

Инструмент показывает это предупреждение на экране применения плана и
требует подтверждения.

### 11.2 Карантин на том же диске

Перенос внутри одного диска — `rename(2)`, мгновенно. Перенос между дисками
или через `/mnt/user/` — полное копирование. Поэтому карантин пишется как
`/mnt/diskN/.photo-quarantine/<зеркало пути>/` на том диске, где лежал файл.
Откат — тоже rename, тоже мгновенный.

### 11.3 Журнал и откат

Каждое действие пишется в `journal` до выполнения: src, dst, размер, blake3.
`undo` проигрывает записи в обратном порядке. Полный хеш проверяется
непосредственно перед переносом — в тот момент, когда корректность важна.
Настоящее удаление — отдельная команда после срока хранения.

### 11.4 Контейнеры

На фазе сканирования `/mnt/disk*` монтируются в Docker read-only.
Запись разрешается только на фазе применения.

### 11.5 Защищённые объекты

- Файлы, упомянутые в живых каталогах Lightroom: могут быть keeper'ом,
  не могут быть кандидатом на удаление без явного снятия защиты.
- Наборы pixel-shift (Sony, 4 кадра со сдвигом сенсора): визуально идентичны,
  SSIM ~0.99. Жёсткий запрет на удаление внутри набора. Детект по
  `SequenceImageNumber` / интервалу в доли секунды.
- `.photoslibrary`: индексируется read-only, внутрь не пишем никогда.
- Companion-группы (RAW + JPEG + xmp + AAE): двигаются только вместе.
- Hardlink / APFS clone: `(dev, inode, nlink)` — удаление не вернёт место.

### 11.6 Ложные срабатывания, требующие защиты

- Однородные кадры (чёрные, белые, засветки) — pHash у всех одинаков.
  Низкая дисперсия → отдельный бакет, автогруппировка запрещена.
- Скриншоты и сканы документов — одинаковая раскладка, разный контент.
  Повышенный порог, обязательная SSIM-верификация.
- EXIF orientation: нормализуем перед хешированием, плюс хешируем 4 поворота.

### 11.7 Стоп-лист

`*.lrdata`, `*.lrprev`, `*.lrcat-data` (**не удалять**, §10.1),
`*.photoslibrary` (кроме read-only индексации), `*/Backups/*.lrcat*`, `@eaDir`, `.DS_Store`, `._*`,
`.Spotlight-V100`, `.fseventsd`, `.TemporaryItems`, `Thumbs.db`,
`.thumbnails`, `.Trash*`, `.recycle`, `node_modules`, `Program Files`,
`Windows`, `AppData`, `.git`, `*/Cache/*`.

---

## 13. Этапы

| этап | содержание |
|---|---|
| Ф0 | **сделано** — инвентаризация и очистка регенерируемых данных (§10), заодно обкатка обхода, карантина, журнала и отката |
| Ф1 | **сделано** — индекс, провенанс, семейства, роли, политика и план |
| Ф2 | pHash + SSIM (T2) **сделано**; эмбеддинги и ORB для T3 — нет |
| Ф3 | серии и выбор лучшего кадра — **сделано**, кроме детекции лиц |
| Ф4 | виды по измеримым признакам **сделано**; zero-shot на эмбеддингах — нет |
| Ф5 | реорганизация структуры — **сделано** |
| Ф6 | инкрементальный режим и ingest |
| Ф7 | видео (755 GiB — крупнейший потенциал по месту) |

Реорганизация выполняется строго после дедупа: команда отказывается
работать, пока план дедупа не пуст (`--allow-duplicates` снимает запрет).
Схема `YYYY/YYYY-MM-DD_<событие>/`, событие — временной кластер с разрывом
более 6 часов. Полный маппинг путей в журнале, операция `organize`.

Определение даты, по убыванию доверия: `DateTimeOriginal` → `CreateDate` →
`DateTime` → дата из имени файла → дата из пути → `mtime`. Источник
показывается пользователю, неуверенные выделяются. Неправдоподобные
значения (до 1990 года — мёртвая батарейка камеры, или будущее) уступают
следующему источнику.

Принято при реализации:

* **Второе и последующие события дня** получают время начала:
  `2019-07-14`, затем `2019-07-14_2000`. Первое остаётся просто датой, и
  архив, где на день приходится одна съёмка, читается как список дат.
* **Собственная схема читается обратно.** На повторном проходе дерево само
  становится входом: `_HHMM` разбирается назад, иначе кадр, у которого
  дата была только из `mtime`, вернулся бы к полуночи и переехал в другое
  событие. Повторный `organize plan` после применения — пустой.
* **Точность даты не выдумывается.** Путь, знающий только месяц или год,
  даёт `2021/2021-06_без-точной-даты/`, а не первое число месяца.
* **GPS и метка CLIP в имя события не вошли.** Без обратного геокодинга
  координаты дают `55.76N-37.62E` — это не название места; метка CLIP ждёт
  pc-embed.
* **Перенос — только `rename(2)` в пределах диска.** Файлы с других дисков
  попадают в отказы: копирование 149 GiB не является переносом.
* **Каталоги Lightroom защищены от переноса,** а не только от удаления:
  ссылка в каталоге ведёт по абсолютному пути и переезда не переживёт.
* **Опустевшие каталоги убираются,** кроме корней обхода и точки
  монтирования; откат убирает за собой не более двух уровней — ровно тех,
  что создал сам (`YYYY/событие`).

Sony нумерует файлы `DSC0xxxx` со сбросом на 9999 — за много лет и несколько
тушек имена массово повторяются у разных кадров. Дедуп по имени запрещён,
реорганизация разводит коллизии суффиксом `_2`, `_3`; суффикс выдаётся на
пару «исходный каталог + основа имени», поэтому RAW и лежащий рядом JPEG
остаются одним кадром, а спутники (`.xmp`, `.aae`, AppleDouble)
переименовываются следом.

---

## 14. Открытые вопросы

1. Природа ~11 800 файлов без расширения вне учтённых `.lrdata` (§2.3).
2. Размеры встроенных превью в ARW по моделям камер — определяет, хватает ли
   их для SSIM и ORB, или для части файлов нужен полный декод.
3. Политика для экспортов Lightroom при живом ARW и каталоге: хранить все
   или чистить по порогу разрешения.
4. Порог `RESIZE` по умолчанию (предложение: удалять производные менее 2 МП,
   если есть представление большего разрешения).
5. Добавление паритетного диска до фазы применения (§12.1).
