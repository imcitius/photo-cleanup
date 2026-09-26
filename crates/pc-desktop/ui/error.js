// The start-up error page. The shell injects the report as
// window.__PC_STARTUP_ERROR__ before this runs; every string goes in with
// textContent, never as HTML, because paths are user data.
(function () {
  "use strict";
  var report = window.__PC_STARTUP_ERROR__ || {
    ru: "Неизвестная ошибка запуска.",
    en: "Unknown start-up error.",
    detail: null
  };
  var ru = /^ru\b/i.test(navigator.language || "");
  var text = {
    title: ru ? "Photo Cleanup не запустился" : "Photo Cleanup could not start",
    hint: ru
      ? "Фотографии не тронуты: до них дело не дошло. Закройте окно, устраните причину и запустите программу снова."
      : "Your photographs are untouched: nothing got that far. Close this window, fix the cause and start the app again.",
    details: ru ? "Подробности" : "Details",
    revert: ru ? "Вернуться к прежней папке данных" : "Go back to the previous data folder",
    retry: ru ? "Повторить" : "Try again",
    revertHint: ru
      ? "Прежняя папка: {0}. Она не удалялась; перенесённая копия тоже остаётся на месте."
      : "Previous folder: {0}. It was not deleted; the moved copy stays where it is too."
  };
  function put(id, value) {
    document.getElementById(id).textContent = value;
  }
  document.documentElement.lang = ru ? "ru" : "en";
  document.title = text.title;
  put("title", text.title);
  put("message", ru ? report.ru : report.en);
  put("other", ru ? report.en : report.ru);
  document.getElementById("other").lang = ru ? "en" : "ru";
  put("hint", text.hint);
  put("details", text.details);
  put("detail", JSON.stringify(report.detail, null, 2));

  // Two actions, through the window's own commands; nothing else is granted
  // to this page. Only "back" changes anything, and only the bootstrap: the
  // folder the app came from is chosen again, no data is copied or removed.
  var ipc = window.__TAURI_INTERNALS__;
  var revert = document.getElementById("revert");
  var retry = document.getElementById("retry");
  var previous = report.detail && report.detail.previous;
  function run(button, command) {
    revert.disabled = retry.disabled = true;
    put("failure", "");
    ipc.invoke(command).catch(function (e) {
      put("failure", String(e));
      revert.disabled = retry.disabled = false;
    });
  }
  if (!ipc) {
    retry.hidden = true;
    return;
  }
  if (previous) {
    var where = previous.data_dir || (ru ? "системная папка программы" : "the app's system folder");
    revert.hidden = false;
    revert.textContent = text.revert;
    revert.title = text.revertHint.replace("{0}", where);
    put("hint", text.hint + " " + text.revertHint.replace("{0}", where));
    revert.addEventListener("click", function () {
      run(revert, "revert_data_dir");
    });
  }
  retry.textContent = text.retry;
  retry.addEventListener("click", function () {
    run(retry, "restart_app");
  });
})();
