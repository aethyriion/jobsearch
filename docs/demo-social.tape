# A second recording, sized for social feeds rather than for GitHub.
#
#   ./docs/demo-data.sh && vhs docs/demo-social.tape
#
# docs/demo.tape is 209 columns at font size 14. GitHub renders that at
# ~880px with click-to-enlarge, so it reads fine. A social feed renders it
# around 552px wide, which puts the effective font at ~4px — a grey smear.
#
# So: half the columns, a much larger font, a shorter beat list, and MP4
# instead of GIF because feeds autoplay and loop video, allow full-screen,
# and compress it far better than 444 GIF frames.
#
# Same throwaway database of fictional companies as the main demo.

Output docs/demo-social.mp4

Set Shell bash
Set FontFamily "JetBrainsMono Nerd Font"
Set FontSize 20
Set Width 2080
Set Height 700
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms

Hide
Type "export JOBSEARCH_DB_PATH=${DEMO_DB:-/tmp/jobsearch-demo/data/seen_jobs.sqlite3}"
Enter
Type "export PATH=$PWD/target/release:$PATH"
Enter
Type "clear"
Enter
Sleep 1s
Show

Type "jobsearch"
Sleep 700ms
Enter
Sleep 2500ms

Down
Sleep 450ms
Down
Sleep 900ms

# startup mode is a view, not a fetch setting
Type "s"
Sleep 1400ms
Type "s"
Sleep 1200ms

# sort by the odds anyone reads the application
Type "S"
Sleep 600ms
Type "S"
Sleep 600ms
Type "S"
Sleep 1500ms

# triage one row
Down
Sleep 400ms
Type "1"
Sleep 1800ms

Type "q"
Sleep 600ms
