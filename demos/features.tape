Output features.gif

Set FontSize 20
Set Theme TokyoNight
Set Padding 2
Set BorderRadius 8
Set TypingSpeed 75ms
Set Width 1200
Set Height 600

# Fixtures: run demos/fixtures/setup-local.sh first. Offline — no network,
# no GitHub account dependency. See demos/create-from-pr.tape for the
# GitHub-PR flow, which can't be faked locally.
#
# Keystrokes below were verified against the running binary (default focus
# is the Repositories pane; 'n' creates a space in the highlighted repo
# directly — no picker step; deleting needs 'X', not Enter, to confirm; the
# Spaces pane must hold focus, via Tab, before its filter/delete/select keys
# do anything). Re-verify with demos/fixtures/setup-local.sh + a throwaway
# `vhs` run before trusting this blindly after a UI change.

Hide
Type@0s "export SHANTI_REPOS_DIR=./repos-dir"
Enter
Type@0s "export SHANTI_WORKTREES_DIR=./worktrees-dir"
Enter
Sleep 2s
Show

Type@0s "tree -d ./worktrees-dir/"
Enter
Sleep 2s

#######################
# Launch shanti     #
#######################

Type@0s "cd $(shanti)"
Sleep 1s
Enter

#############################################
# Create a space in backend-repo (highlighted
# by default — 'n' makes it there directly)
#############################################

Type "n"
Sleep 800ms
Type@0s "add-backend-api-for-the-new-button"
Sleep 600ms
Enter
Sleep 800ms

#############################################
# Create a space in frontend-repo
#############################################

Type "j"
Sleep 500ms
Type "n"
Sleep 800ms
Type@0s "add-new-button-to-the-ui"
Sleep 600ms
Enter
Sleep 800ms

Type "k"
Sleep 500ms

#############################################
# Show help popup
#############################################

Type "?"
Sleep 2s
Escape

#############################################
# Focus the spaces pane, filter its list
#############################################

Tab
Sleep 500ms
Type "i"
Sleep 500ms
Type@0s "back"
Sleep 800ms
Backspace
Backspace
Backspace
Backspace
Escape
Sleep 1s

#############################################
# Delete a space (soft delete with confirm)
#############################################

Type "d"
Sleep 1s
Type "X"
Sleep 1500ms

#############################################
# Back to frontend-repo's space, select it —
# copies path and exits
#############################################

Tab
Sleep 500ms
Type "j"
Sleep 500ms
Tab
Sleep 500ms
Enter
Sleep 1s

Type@0s "pwd"
Enter
Sleep 1s

Type@0s "cd -"
Enter
Sleep 1s

Type@0s "tree -d worktrees-dir/"
Enter
Sleep 4s
