# VHS tape for the README demo GIF.
#
#   ./docs/demo-data.sh && vhs docs/demo.tape
#
# Regenerate this whenever the TUI's layout or keys change — a stale GIF is
# worse than none. It needs a database to show, and it must NOT be a real
# one: `docs/demo-data.sh` builds a throwaway database of fictional
# companies for exactly this purpose. Run that first.
#
# Everything between Hide and Show is setup the viewer never sees.

Output docs/demo.gif

Set Shell bash
Set FontFamily "JetBrainsMono Nerd Font"
Set FontSize 14
Set Width 1760
Set Height 880
Set Padding 22
Set Theme "Catppuccin Mocha"
Set TypingSpeed 55ms

Hide
Type "export DB_PATH=${DEMO_DB:-/tmp/jobsearch-demo/data/seen_jobs.sqlite3}"
Enter
Type "export PATH=$PWD/target/release:$PATH"
Enter
Type "clear"
Enter
Sleep 1s
Show

# --- the hook: one command, and you are triaging -----------------------
Type "jobsearch"
Sleep 800ms
Enter
Sleep 2500ms

# --- move through the list, detail pane follows ------------------------
Down
Sleep 500ms
Down
Sleep 500ms
Down
Sleep 900ms

# --- startup mode is a VIEW, not a fetch setting -----------------------
Type "s"
Sleep 1600ms
Type "s"
Sleep 1400ms

# --- sort by callback odds: will anyone actually read this? ------------
Type "S"
Sleep 700ms
Type "S"
Sleep 700ms
Type "S"
Sleep 1600ms

# --- triage: mark the selected row applied ----------------------------
Down
Sleep 400ms
Type "1"
Sleep 1600ms

Screenshot docs/screenshot.png

# --- the help overlay, so the keymap is visible ------------------------
Type "?"
Sleep 2600ms
Escape
Sleep 500ms

Type "q"
Sleep 800ms
