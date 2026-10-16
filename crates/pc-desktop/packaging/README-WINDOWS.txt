photo-cleanup — настольное приложение для Windows (тестовая сборка)
====================================================================

Что это
-------
Пробная сборка настольного окна photo-cleanup для ручной проверки на
настоящей Windows. Это не релиз: сборка не подписана, не устанавливается
и не обновляется сама.

В архиве:
  photo-cleanup-desktop.exe  — приложение с окном (интерфейс встроен в exe,
                               интернет не нужен);
  photo-cleanup.exe          — то же самое в командной строке; для проверки
                               окна не нужен;
  LICENSE                    — лицензия;
  README-WINDOWS.txt         — этот файл.

Что нужно
---------
- Windows 10 или 11, 64 бит (x64).
- Microsoft Edge WebView2 Runtime. На Windows 11 и обновлённой Windows 10
  он уже есть. Если после запуска не появилось ни окна, ни сообщения —
  скорее всего, его нет: установите «Evergreen Runtime» со страницы
  Microsoft WebView2 и запустите снова. Загрузчика WebView2 в архиве нет.
  (Отдельного сообщения на этот случай пока нет — это известный пробел.)
- Microsoft Visual C++ Redistributable (x64), 2015–2022: обе программы
  используют его библиотеки VCRUNTIME140.dll и VCRUNTIME140_1.dll. Почти
  всегда он уже установлен. Если Windows пишет, что такой DLL не найдено,
  установите «Microsoft Visual C++ Redistributable» x64 с сайта Microsoft.

Запуск
------
Распакуйте архив в любую папку и дважды щёлкните
photo-cleanup-desktop.exe.

Сборка не подписана, поэтому Windows SmartScreen покажет «Windows защитила
ваш компьютер». Нажмите «Подробнее», затем «Выполнить в любом случае».
Если архив скачан браузером, Windows может также спросить про файл из
интернета — это то же самое предупреждение.

Где лежат данные
----------------
По умолчанию — в профиле пользователя, не рядом с программой:
  %LOCALAPPDATA%\io.github.imcitius.photo-cleanup\data\
    photo-cleanup.db (+ -wal, -shm во время работы) — база;
    photo-cleanup.db.writer-lock — файл замка (остаётся и после выхода,
                                   это нормально);
    thumbs\ — миниатюры.
Выбор папки данных хранится в
  %LOCALAPPDATA%\io.github.imcitius.photo-cleanup\desktop.json
Чтобы начать заново, спросите того, кто прислал сборку.

Копирование данных программы в другую папку (кнопка «Сменить папку…» в
разделе «Папка данных программы») на Windows намеренно отключено:
программа пока не может доказать, что другие учётные записи не вмешаются
во временные папки переноса, поэтому отказывает заранее, ничего не
записав. Такой отказ — ожидаемое поведение, а не ошибка.

Как выйти
---------
Крестик окна только прячет окно — работа и сервер продолжаются. Значок в
области уведомлений (трей, у часов): «Открыть» — вернуть окно,
«Остановить работу» — остановить текущую задачу, «Выйти» — закрыть
приложение. Если идёт задача, «Выйти» спросит, ждать её или остановить.

Что программа делает с фотографиями
-----------------------------------
Ничего не удаляется — на Windows программа не удаляет ни одного файла
никогда. Окончательная очистка карантина (purge) здесь отключена навсегда:
она отказывает с объяснением, ничего не тронув.

Что работает на дисках NTFS (обычный диск Windows):
  - перенос выбранных копий в карантин — скрытую папку
    .photo-cleanup-quarantine рядом с файлом или в заданной папке на том
    же диске; снимок переезжает вместе со своими .xmp/.aae;
  - откат (возврат) завершённого переноса на прежнее место.
Перед каждым переносом файл перепроверяется; если он изменился, перенос
отказывает, и файл остаётся на месте.

Что пока отказывает с объяснением (это ожидаемо, не ошибка; ни один файл
при этом не сдвигается):
  - файл открыт другой программой (Lightroom, антивирус, просмотр в
    Проводнике) — сообщение «…открыт другой программой… закройте её и
    повторите»; закройте программу и запустите перенос ещё раз (отказ
    остаётся в журнале с этой причиной);
  - рядом со снимком под именем его .xmp/.aae лежит не обычный файл, а
    ярлык-ссылка, junction или папка — снимок не переносится вместе со
    всеми спутниками; замените ссылку самим файлом или уберите её;
  - откат, если на месте прежней папки снимка теперь ссылка или junction —
    всё остаётся в карантине, откат можно повторить, когда там снова
    обычная папка;
  - диск не NTFS (флешки и карты с exFAT или FAT, ReFS), а также файлы
    OneDrive «только в облаке»;
  - карантин на другом диске, чем сами снимки;
  - сверка после сбоя (прерванная операция), возврат файлов, о которых
    журнал не знает, и раскладка по папкам.
Если что-то из этого нужно — сообщите, это будет включено отдельно.

Для пробы возьмите копию небольшой папки со снимками, а не весь архив.
Проверьте: перенос в карантин и откат возвращают всё на место (вместе с
.xmp), а каждый отказ понятно объясняет, что не так.

Что сообщить
------------
Версию Windows, что делали, что ожидали и что получилось; снимок экрана
при ошибке (на экране ошибки есть «подробности» — их текст тоже).

Исходный код и документация: https://github.com/imcitius/photo-cleanup


-----------------------------------------------------------------------
English (short)
-----------------------------------------------------------------------
Test build of the photo-cleanup desktop window for manual checking on
Windows. Not a release: unsigned, nothing is installed.

- Needs 64-bit Windows 10/11 and the Microsoft Edge WebView2 Runtime
  (present on Windows 11 and updated Windows 10). If neither a window nor a
  message appears, install the WebView2 "Evergreen Runtime" from Microsoft.
- Also needs the Microsoft Visual C++ Redistributable x64 (2015-2022,
  VCRUNTIME140.dll / VCRUNTIME140_1.dll); usually already installed.
- Unzip anywhere, double-click photo-cleanup-desktop.exe. SmartScreen will
  warn because the build is unsigned: "More info" -> "Run anyway".
- Data: %LOCALAPPDATA%\io.github.imcitius.photo-cleanup\data\ (database
  photo-cleanup.db, its writer-lock file, thumbs\). The data-folder choice
  is in %LOCALAPPDATA%\io.github.imcitius.photo-cleanup\desktop.json.
  To start over, ask whoever sent you the build.
- Copying the app data to another folder ("Change folder...") is refused
  on Windows by design, before anything is written. That is expected.
- The window's close button only hides it. Quit from the tray icon:
  "Quit" ("Open" brings the window back, "Stop work" stops the current job).
- Nothing is ever deleted on Windows: purge (emptying the quarantine for
  good) is disabled there for good and refuses, touching nothing.
- On NTFS drives, moving the chosen copies into quarantine (a hidden
  .photo-cleanup-quarantine folder beside them, or a folder you set on the
  same drive), together with their .xmp/.aae, works, and so does undoing a
  finished move. Each file is checked again right before it moves.
- Refused with a reason, nothing moved (expected, not a bug): a file open
  in another program (Lightroom, an antivirus, Explorer's preview) - close
  it and try again; drives that are not NTFS (exFAT/FAT cards and sticks,
  ReFS) and OneDrive online-only files; a quarantine on another drive;
  a link, junction or folder bearing the name of a photo's .xmp/.aae (the
  photo and its companions stay; replace the link with the file itself);
  an undo whose original folder is now a link or junction (everything
  stays in quarantine and can be undone once it is a plain folder again);
  recovery after a crash, returning files the journal does not know, and
  sorting into folders. Try it on a copy of a small folder, not your whole
  archive, and check that undo brings everything back.
- photo-cleanup.exe is the command-line tool; not needed for this test.
