@echo off
rem Start photo-cleanup and open it in a browser.
rem
rem The index and the thumbnail cache are written next to this file, so the
rem whole thing stays in one folder you can move or delete.
cd /d "%~dp0"
photo-cleanup.exe --db "%~dp0photo-cleanup.db" serve --thumbs "%~dp0thumbs" --open
pause
