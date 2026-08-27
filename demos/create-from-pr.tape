Output create-from-pr.gif

Set FontSize 20
Set Theme TokyoNight
Set Padding 2
Set BorderRadius 8
Set TypingSpeed 75ms
Set Width 1200
Set Height 600

# Fixtures: run demos/fixtures/setup-github.sh first. Needs network, SSH
# auth, and the muzomer/first-repo + muzomer/second-repo fixture repos (with
# open PRs) to still exist — this is the one flow that can't be faked with
# local-only repos, since shanti calls the real GitHub API. See
# demos/features.tape for the offline demos.

Hide
Type@0s "export SHANTI_REPOS_DIR=./repos-dir"
Enter
Type@0s "export SHANTI_SPACES_DIR=./spaces-dir"
Enter
Sleep 2s
Show

Type@0s "cd $(shanti)"
Sleep 1s
Enter

#############################################
# Create a space from a PR link (manual repo pick)
#############################################

Type "p"
Sleep 1s

Type@0s "https://github.com/muzomer/second-repo/pull/2"
Sleep 1s

Enter
Sleep 1s

Enter
Sleep 2s

Enter
Sleep 2s

#############################################
# Create a space from a PR link (auto-clone)
# P = Shift+P — skips all confirmation prompts
#############################################

Type "P"
Sleep 1s

Type@0s "https://github.com/muzomer/first-repo/pull/1"
Sleep 500ms
Enter
Sleep 2s

Type@0s "tree -d spaces-dir/"
Enter
Sleep 4s
