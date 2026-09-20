photo-cleanup
=============

Sorting out a large photo archive: duplicates and the versions of one shot,
bursts, Lightroom previews, and sorting what is left by date.

To start: double-click "Start photo-cleanup.bat".
The browser opens at http://127.0.0.1:8080 by itself.

Everything stays in this folder: photo-cleanup.db holds the index and
thumbs\ holds the previews. Move the folder or delete it and nothing is
left behind.

Nothing is deleted from your archive without a confirmation. Files you
choose to remove move into a hidden .photo-cleanup-quarantine folder beside
themselves, on the same drive, and come back with one button.

To close it: close the console window that opened.

Source and documentation: https://github.com/imcitius/photo-cleanup
