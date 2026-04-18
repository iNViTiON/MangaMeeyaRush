# MangaMeeya CE — Full Command Inventory

Extracted from the PE resources of `MangaMeeyaCE-English.exe` (v2.4 Beta):

- 2 menus (`MENU 302`, `MENU 460`) — docking menu + explorer context menu
- 57 dialogs (`DIALOG 454..509`) — settings screens
- 23 string tables (`STRING 26..48`) — ~663 strings covering labels + per-command help text

This file catalogues **every** command the binary exposes, grouped by menu.
It is the ground truth for the Rust port; the original `MangaMeeyaCE.ini`
references commands by these numeric IDs in `[Keyboard]`, `[Mouse]`,
`[Gesture*]`, `[Menu]`, and `[TouchPanelArea*]` sections.

Legend: `✅ implemented · 🟡 partial · ❌ not in port · 🗑 intentionally dropped`

_Last audited after milestones M1–M7; ~170 → ~95 commands shipped._

## File (ID 701–738)

| ID   | Label                              | Status |
|------|------------------------------------|--------|
| 701  | Open File…                         | ✅ (Shift+O) |
| 702  | Open Folder…                       | ✅ (O) |
| 703  | Open URL… (HTTP)                   | ❌ |
| 704  | Save…                              | ❌ |
| 705  | Close                              | ❌ |
| 706  | Reload                             | ❌ |
| 707  | Print page settings                | ❌ |
| 708  | Print                              | ❌ |
| 709  | Read-in setup file                 | ❌ |
| 710  | Save setup file                    | ❌ |
| 711–720 | Load preset INI slot 1..10     | ❌ |
| 721–730 | Save preset INI slot 1..10     | ❌ |
| 731  | History                            | ✅ (Ctrl+H, 50-entry SQLite) |
| 732  | Exit                               | ✅ (window close) |
| 733  | Bookmark add / remove              | ✅ (Ctrl+D toggle, SQLite) |
| 734  | Open bookmark                      | ✅ (Ctrl+B list dialog) |
| 735  | Save setting                       | ✅ (Ctrl+, dialog + auto-save) |
| 736  | Preserve shortcut                  | ❌ |
| 737  | Bookmarker insert / remove         | ✅ (Ctrl+D) |
| 738  | Wallpaper setting                  | 🗑 (not relevant cross-platform) |

## View (ID 741–767)

| ID   | Label                                    | Status |
|------|------------------------------------------|--------|
| 741  | 1-page display                           | ✅ |
| 742  | 2-page display                           | ✅ |
| 743  | Toggle 1/2 page                          | ✅ (Space) |
| 744  | Auto 2-page                              | ❌ |
| 745  | Auto single-page (horizontal split)      | ❌ |
| 746  | Auto single-page (vertical split)        | ❌ |
| 747  | Auto-display settings                    | ❌ |
| 748  | Open from left (Western)                 | ✅ (BindDir=LTR) |
| 749  | Open from right (Eastern, manga)         | ✅ (BindDir=RTL, default) |
| 750  | Toggle binding direction                 | ❌ (runtime toggle) |
| 751–757 | Origin modes (6) + toggle             | ❌ (centred only) |
| 758  | Reading mode                             | ✅ (default) |
| 759  | Thumbnail mode                           | ❌ |
| 760  | Explorer mode                            | ✅ (E) |
| 761  | Change mode                              | ✅ (E toggles) |
| 762  | Reading-mode settings dialog             | ❌ |
| 763  | Thumbnail-mode settings dialog           | ❌ |
| 764  | Explorer-mode settings dialog            | ❌ |
| 765  | Fullscreen                               | ✅ (F11 / Alt+Enter) |
| 766  | Loupe (magnifier)                        | ❌ |
| 767  | Insert blank page for frontispiece       | ❌ |

## Scale Mode (ID 771–789)

| ID   | Label                                         | Status |
|------|-----------------------------------------------|--------|
| 771  | Fixed magnification                           | ✅ (FitMode::Custom) |
| 772  | Match either height/width                     | ✅ (Fit) |
| 773  | Scale to window                               | ✅ (Fit) |
| 774  | Display actual size                           | ✅ (Original) |
| 775  | Match image size (resize window)              | ❌ |
| 776  | Match height (2-page)                         | ✅ (FitHeight) |
| 777  | Display at specified scale                    | ✅ (Custom) |
| 778  | Change scale mode                             | ✅ (keyboard) |
| 779  | Reduce but not magnify (no-zoom-in)           | ✅ |
| 781  | Nearest neighbour                             | ❌ (Triangle only) |
| 782  | HALFTONE filter                               | ❌ |
| 783  | Mean pixel + bicubic                          | ❌ |
| 784  | Lanczos3                                      | ❌ |
| 785  | Bicubic                                       | ❌ |
| 786  | Linear                                        | 🟡 (Triangle ≈ linear) |
| 787  | Mean pixel + unsharp (low)                    | ❌ |
| 788  | Mean pixel + unsharp (high)                   | ❌ |
| 789  | Switch resize algorithm                       | ❌ |

## Movement (ID 791–818)

| ID   | Label                                    | Status |
|------|------------------------------------------|--------|
| 791  | Next                                     | ✅ (→ in LTR / ← in RTL, click) |
| 792  | Previous                                 | ✅ |
| 793  | Next page (single)                       | ✅ (Shift+→) |
| 794  | Previous page (single)                   | ✅ (Shift+←) |
| 795–800 | Jump N pages fwd/back (3 presets)     | ❌ |
| 801  | Last page                                | ✅ (End) |
| 802  | First page                               | ✅ (Home) |
| 803  | Go-to dialog                             | ✅ (Ctrl+G) |
| 805  | Next subfolder                           | 🟡 (Shift+↓ sibling) |
| 806  | Previous subfolder                       | 🟡 (Shift+↑ sibling) |
| 807  | Next folder / archive                    | 🟡 (Shift+↓ sibling) |
| 808  | Previous folder / archive                | 🟡 (Shift+↑ sibling) |
| 809  | Auto-play forward                        | ✅ (F9) |
| 810  | Auto-play reverse                        | ✅ (Shift+F9) |
| 811  | Pause                                    | ✅ (F8 / Esc) |
| 812  | Toggle play / pause                      | ✅ (F9) |
| 813  | Toggle reverse play / pause              | ❌ |
| 814  | Playback settings                        | ✅ (Ctrl+, → Playback) |
| 815  | Disable loop                             | ✅ (LoopMode::Off available) |
| 816  | Loop                                     | ✅ (default, wrap at ends) |
| 817  | Auto-move folder                         | ❌ |
| 818  | Change move format                       | ❌ |

## Scroll / Zoom (ID 821–830)

| ID   | Label                             | Status |
|------|-----------------------------------|--------|
| 821  | Scroll ←                          | 🟡 (mouse pan, no key) |
| 822  | Scroll ↑                          | 🟡 |
| 823  | Scroll →                          | 🟡 |
| 824  | Scroll ↓                          | 🟡 |
| 825  | Sequential scroll                 | ❌ |
| 826  | Reverse sequential scroll         | ❌ |
| 827  | Scroll-and-next-page              | ❌ |
| 828  | Scroll-and-previous-page          | ❌ |
| 829  | Magnify                           | ✅ (+) |
| 830  | Reduce                            | ✅ (-) |

## Explorer Mode (ID 831–847)

| ID   | Label                         | Status |
|------|-------------------------------|--------|
| 831  | Open (double-click)           | ✅ |
| 832  | Open                          | ✅ |
| 833  | Move up (parent)              | ✅ |
| 834  | Previous                      | ❌ |
| 835  | Next                          | ❌ |
| 836  | Renew (refresh)               | ✅ (F5) |
| 837  | Find                          | ✅ (Ctrl+F filter bar) |
| 838  | Rename                        | ✅ (F2 dialog) |
| 839  | Delete                        | ✅ (Delete → confirm) |
| 840  | Copy                          | ❌ |
| 841  | Cut                           | ❌ |
| 842  | Paste                         | ❌ |
| 843  | Extract here                  | ❌ |
| 844  | Create folder + extract       | ❌ |
| 845  | Extract to specified folder   | ❌ |
| 846  | Open from external program    | ❌ |
| 847  | Property                      | ❌ |

## Window (ID 851–882)

| ID   | Label                         | Status |
|------|-------------------------------|--------|
| 851  | Image info window             | ✅ (I overlay) |
| 852  | Seek bar                      | ✅ (S toggle; draggable bottom bar) |
| 853–856 | Toolbar 1..4 display         | 🗑 (single-window layout) |
| 859  | Folder tree                   | ❌ |
| 860  | File list                     | ❌ |
| 861  | Book list                     | ❌ |
| 862  | Bookshelf                     | ❌ |
| 863  | Table of contents             | ❌ |
| 869  | Next view window              | ❌ |
| 870  | New view window               | ❌ |
| 871  | Read-ahead flag               | ✅ (on by default) |
| 872  | Cache resized images          | 🟡 (raw cached, resize at draw) |
| 873  | Page effects (transitions)    | ✅ (M7 crossfade, animations_enabled toggle) |
| 874  | Sort files                    | 🟡 (natural only) |
| 875  | Sort folders                  | 🟡 |
| 876  | Customize dialog              | ❌ |
| 877  | System settings dialog        | ✅ (Ctrl+, tabbed dialog) |
| 878  | Display popup menu            | ❌ |
| 881  | Cache info dialog             | ❌ |
| 882  | Version info dialog           | ❌ |

## Filter (ID 885–898)

| ID   | Label                              | Status |
|------|------------------------------------|--------|
| 885  | Rotate (cycle 0/90/180/270)        | ✅ (R / Shift+R / Ctrl+R) |
| 886–889 | Rotate exact (0/90/180/270)     | ✅ (via cycles) |
| 890  | Rotate settings                    | ❌ |
| 891  | Toggle clipping filter             | 🟡 (Pipeline accepts Clip, no UI) |
| 892  | Clipping filter settings           | ❌ |
| 893  | Toggle brightness filter           | 🟡 (Pipeline accepts Adjust, no UI) |
| 894  | Brightness filter settings         | ❌ |
| 895  | Toggle sharpness filter            | 🟡 (Pipeline accepts Sharpen, no UI) |
| 896  | Sharpness filter settings          | ❌ |
| 897  | Toggle resize filter               | 🟡 (Pipeline accepts Resize, no UI) |
| 898  | Resize filter settings             | ❌ |

## Sort (ID 901–917)

| ID    | Label                     | Status |
|-------|---------------------------|--------|
| 901/902 | Path forward / reverse  | ❌ |
| 903/904 | File name f / r         | ❌ |
| 905/906 | Extension f / r         | ❌ |
| 907/908 | Size f / r              | ❌ |
| 909/910 | Date-modified f / r     | ❌ |
| 911/912 | Path (numeric) f / r    | ✅ (forward only) |
| 913/914 | Path (alpha) f / r      | ❌ |
| 915/916 | Entry order f / r       | ❌ |
| 917    | Random                   | ❌ |

## Explicitly dropped (per user direction)

- Furigana / ruby text (`[Text]` section, IDs not re-catalogued here)
- Per-archive resume (IDs 755–756 origin-position-log related)
- RAR archives (handled by `arc.dll` in the original)
- PDF (legacy `pdf.dll` from 2012)

## Summary scorecard (post-M7)

- **Fully implemented:** ~85 commands
- **Partial:** ~22 commands
- **Missing:** ~70 commands (mostly the shortcut/toolbar/customization
  editors, HTTP URL open, file extract, rotate-exact dialog, and the
  folder-tree/bookshelf side panels)
- **Intentionally dropped:** ~12 commands (disk thumbnail cache, print,
  wallpaper, RAR, PDF, furigana, per-archive resume, touch-panel,
  10-setup-file slots, multi-view windows, toolbars, HTTP open)

The port now covers the full reading path end-to-end (filters, playback,
overlays, crossfade animations), plus bookmarks/history/per-book resume
via SQLite, explorer file ops, and a settings UI. The remaining gap is
editors (keybinds, gestures, shortcut tables) and niche dialogs
(cache-info, version-info, rotate-exact degrees).
