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
Чтобы начать с чистого листа, закройте приложение (см. «Как выйти») и
удалите папку %LOCALAPPDATA%\io.github.imcitius.photo-cleanup целиком.
Ваши фотографии там не лежат.

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
Ничего не удаляется. Файлы, которые вы решили убрать, переносятся в
скрытую папку .photo-cleanup-quarantine рядом с ними, на том же диске, и
возвращаются одной кнопкой. Место освобождает только отдельная очистка
карантина (purge) — для проверки её запускать не нужно и не надо.
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
  Deleting that folder after quitting starts from scratch; no photos are
  kept there.
- Copying the app data to another folder ("Change folder...") is refused
  on Windows by design, before anything is written. That is expected.
- The window's close button only hides it. Quit from the tray icon:
  "Quit" ("Open" brings the window back, "Stop work" stops the current job).
- Nothing is deleted: files you remove go to a hidden
  .photo-cleanup-quarantine folder beside them on the same drive and can
  be restored. Only a separate purge frees space — do not run it for this
  test. Try it on a copy of a small folder, not your whole archive.
- photo-cleanup.exe is the command-line tool; not needed for this test.
