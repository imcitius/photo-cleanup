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

## Запуск на NAS

Образ, собранный на Маке, живёт в демоне Мака — на NAS это другая машина
и другой Docker. Сборку надо доставить.

Самый простой путь: **собрать на самом NAS и получить статический бинарь**,
который запускается без контейнера вообще.

```bash
scripts/deploy-to-nas.sh root@tower /mnt/user/appdata/photo-cleanup
```

Скрипт копирует исходники по rsync, запускает сборку там (архитектура
совпадает, эмуляции нет) и оставляет `/mnt/user/appdata/photo-cleanup/photo-cleanup`.

Вручную то же самое:

```bash
rsync -az --delete --exclude target --exclude .git . root@tower:/mnt/user/appdata/photo-cleanup/src/
```

```bash
ssh root@tower "cd /mnt/user/appdata/photo-cleanup/src && docker build --target export --output type=local,dest=.. ."
```

Бинарь статический (musl, SQLite вкомпилирован), зависимостей нет — на Unraid
переживает обновления ОС.

### Если собирать всё-таки на Маке

Тогда образ нужно перенести целиком:

```bash
docker build --platform linux/amd64 -t photo-cleanup:dev .
```

```bash
docker save photo-cleanup:dev | gzip | ssh root@tower "gunzip | docker load"
```

Либо выгрузить один бинарь и скопировать его:

```bash
docker build --platform linux/amd64 --target export --output type=local,dest=./dist .
```

```bash
scp dist/photo-cleanup root@tower:/mnt/user/appdata/photo-cleanup/
```

### Запуск в контейнере

```bash
docker run --rm \
  -v /mnt/disk1:/mnt/disk1:ro -v /mnt/disk2:/mnt/disk2:ro -v /mnt/disk3:/mnt/disk3:ro \
  -v /mnt/cache/appdata/photo-cleanup:/db \
  photo-cleanup:dev --db /db/pc.db scan --root /mnt/disk3/data/media/foto
```

На фазе сканирования монтируйте диски read-only; запись нужна только для
`clean`, `undo` и `purge`.

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
