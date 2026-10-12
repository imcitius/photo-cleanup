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
Чтобы начать с чистого листа, закройте приложение (см. «Как выйти»),
откройте эти папки и проверьте, что в них лежат только перечисленные
выше файлы: photo-cleanup.db, photo-cleanup.db-wal, photo-cleanup.db-shm,
photo-cleanup.db.writer-lock, папка thumbs\ и desktop.json. Тогда
удалите только эти файлы и папку thumbs\, ничего больше. Если там есть
что-то ещё или вы не уверены — ничего не удаляйте и спросите того, кто
прислал сборку.

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
Ничего не удаляется. В этой сборке для Windows перенос файлов в карантин,
откат (возврат) и сверка после сбоя намеренно отключены: программа
отказывает заранее, ничего не сдвинув, с сообщением вроде
  «на Windows перенос, откат и сверка пока не выполняются: проверка
  личности файлов и безопасного удаления временных файлов на настоящей
  системе Windows ещё не проведена»
(или его английский вариант). Такой отказ — ожидаемое поведение, а не
ошибка. Проверьте, что сообщение понятно и что ни один файл не сдвинулся
с места. Место освобождает только отдельная очистка карантина (purge) —
для проверки её запускать не нужно и не надо.
Для пробы лучше взять копию небольшой папки со снимками, а не весь архив.

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
  To start from scratch: quit the app, check that those folders hold only
  photo-cleanup.db, photo-cleanup.db-wal, photo-cleanup.db-shm,
  photo-cleanup.db.writer-lock, thumbs\ and desktop.json, then delete
  only those. If anything else is there or you are unsure, delete nothing
  and ask whoever sent you the build.
- Copying the app data to another folder ("Change folder...") is refused
  on Windows by design, before anything is written. That is expected.
- The window's close button only hides it. Quit from the tray icon:
  "Quit" ("Open" brings the window back, "Stop work" stops the current job).
- Nothing is deleted. In this Windows build, moving files to quarantine,
  undo and recovery are disabled by design: the app refuses before
  anything moves, with a message like "on Windows, moves, undo and
  recovery are not carried out yet: file identity and the safe cleanup of
  temporary files have not been verified on a real Windows system" (or
  its Russian version). That refusal is expected; check that it is clear
  and that no file has moved. Only a separate purge frees space — do not
  run it for this test. Try it on a copy of a small folder, not your
  whole archive.
- photo-cleanup.exe is the command-line tool; not needed for this test.
