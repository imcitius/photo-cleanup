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
    details: ru ? "Подробности" : "Details"
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
})();
