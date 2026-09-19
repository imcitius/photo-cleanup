# photo-cleanup

Разбор большого фотоархива: дубликаты, серии, регенерируемые данные.
Проект и обоснование решений — [DESIGN.md](DESIGN.md).

Реализована **фаза 0**: опись архива и очистка регенерируемых данных —
превью Lightroom, кэши, системный мусор. Дедуп изображений идёт следующим
этапом (§13 дизайна).

## Что делает фаза 0

* обходит дерево, шардируя работу по физическим дискам;
* распознаёт бандлы производных данных по имени каталога и **не заходит
  внутрь** — вместо 24 тысяч файлов превью в описи одна строка;
* читает каталоги Lightroom только на чтение, по копии, и использует их для
  проверок безопасности;
* переносит выбранное в карантин на том же диске (`rename`, мгновенно);
* освобождает место отдельной командой после срока удержания.

## Правила безопасности

| | |
|---|---|
| `*.lrcat-data` | **никогда не удаляется** — ИИ-маски и Denoise, регенерации нет. Запрет на уровне кода, политикой не снимается |
| открытый каталог | есть `*.lrcat.lock` → Lightroom работает с каталогом → бандл заблокирован |
| Smart Previews | удаляются, только если **все** мастера каталога найдены на диске |
| изменился с момента сканирования | пропускается, не переносится |
| карантин на другом диске | отказ: перенос превратился бы в копирование |
| удаление | только `purge`, только явным `--yes`, только после срока удержания |

Ничего не удаляется командой `clean` — данные лежат в карантине и
возвращаются одним `undo`.

## Быстрый старт

```bash
cargo build --release
```

```bash
./target/release/photo-cleanup --db /mnt/cache/appdata/photo-cleanup/pc.db \
  scan --root /mnt/disk1/data/media/foto \
       --root /mnt/disk2/data/media/foto \
       --root /mnt/disk3/data/media/foto
```

Корни задавать через `/mnt/diskN`, **не** через `/mnt/user` — FUSE-слой
скрывает, на каком диске лежит файл, и ломает и шардинг, и карантин.

```bash
photo-cleanup derived list          # что найдено и что заблокировано
photo-cleanup catalogs              # каталоги Lightroom
photo-cleanup status                # сводка и содержимое карантина
```

```bash
photo-cleanup derived clean --kind lr-previews --dry-run
photo-cleanup derived clean --kind lr-previews --yes
photo-cleanup derived undo --journal 12
photo-cleanup derived purge --older-than 7d --yes
```

Виды: `lr-previews`, `lr-smart-previews`, `lr-helper`, `lr-lrdata-other`,
`system-junk`. Вид `lr-catalog-data` существует в описи, но недоступен для
удаления.

## Docker

```bash
docker buildx build --platform linux/amd64 -t photo-cleanup:dev --load .
```

```bash
docker run --rm \
  -v /mnt/disk1:/mnt/disk1 -v /mnt/disk2:/mnt/disk2 -v /mnt/disk3:/mnt/disk3 \
  -v /mnt/cache/appdata/photo-cleanup:/db \
  photo-cleanup:dev --db /db/pc.db scan --root /mnt/disk3/data/media/foto
```

На фазе сканирования монтируйте диски read-only (`-v /mnt/disk3:/mnt/disk3:ro`);
запись нужна только для `clean`, `undo` и `purge`.

## Структура

```
pc-core       типы, определение диска и точки монтирования, форматирование
pc-db         SQLite: схема, миграции, запросы
pc-walk       обход с шардингом по дискам, распознавание бандлов
pc-lightroom  чтение .lrcat только на чтение, проверка наличия мастеров
pc-apply      карантин, откат, purge, журнал
pc-cli        команды
```

## Разработка

```bash
cargo test && cargo clippy --all-targets && cargo fmt --all --check
```

Сквозные тесты (`crates/pc-cli/tests/e2e.rs`) собирают дерево, повторяющее
реальный архив — живые каталоги, открытый каталог, smart previews с
потерянными мастерами, сирота, защищённые данные, настоящие фотографии — и
проверяют полный цикл вместе с тем, что защищённое осталось нетронутым.
