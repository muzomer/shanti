# Shanti

Shanti manages many concurrent lines of work across many repositories, giving
each its own checked-out directory. This file fixes the words for that, so the
same idea is not called three things in three places.

Terms only — no file paths and no implementation. How the code is arranged is
discoverable from the code; what a word *means* is not.

## Language

**Space**:
One checked-out directory of a repository, workable independently of the others.
_Avoid_: worktree, workspace — each names one backend's version of this, and the
UI holds spaces from both at once.

**Repository**:
A version-controlled project found on disk, tagged with the backend that drives
it.
_Avoid_: repo (in prose), project.

**Backend**:
The version control system driving a repository — git or jujutsu.
_Avoid_: VCS provider, driver.

**Colocated repository**:
A repository that both git and jujutsu can drive. jujutsu owns it; git remains
available so worktrees made outside shanti are still listed.
_Avoid_: hybrid, dual repo.

**Space status**:
What a space looks like right now, in two halves: its relationship to the
upstream, and its local state. The upstream half means the same for both
backends; the local half does not, because jujutsu records edits as it goes and
git does not.
_Avoid_: state, health.

**Tone**:
How much attention a status reading deserves, from muted to danger. A judgement
about meaning, never about colour.
_Avoid_: severity (that belongs to notifications), level.

**Space tip**:
The most recent commit — or jujutsu change — made in a space, with the moment it
was made. Its *age* is derived when drawn, never stored.
_Avoid_: head, latest.

**Deletion risk**:
What deleting a space would cost, and whether anything could bring it back. The
one question asked before anything is destroyed.
_Avoid_: safety check, danger level.

**Hook**:
A command the user configured to run once, after a space is created. A hook that
works is invisible.
_Avoid_: script, post-install, task.

**Modal**:
A popup layer that owns the keyboard while it is on top. Stacked, so closing one
reveals the layer beneath.
_Avoid_: dialog, popup (in code), overlay.

**Notification**:
A short message from the app to the user, with a loudness and an expiry. Distinct
from anything GitHub sends — see *Flagged ambiguities*.
_Avoid_: toast, alert, message.

**Scheme**:
A named colour palette the user can choose and persist.
_Avoid_: theme (that is the value a scheme resolves to), skin.

## Relationships

- A **Repository** is driven by one or more **Backends**; more than one means it
  is **Colocated**.
- A **Repository** has zero or more **Spaces**.
- A **Space** belongs to exactly one **Repository** and one **Backend**.
- A **Space** has one **Space status** and one **Space tip**.
- A **Space status** yields a **Deletion risk**.
- Every status reading carries a **Tone**.
- Creating a **Space** may run **Hooks**.

## Flagged ambiguities

- **"notification"** means two different things once GitHub is involved: the
  app's own short message to the user, and an item GitHub sends about a pull
  request. Resolved: **Notification** is the app's; the GitHub one is an
  **Inbox item**, and the surface listing them is the **Inbox**.

- **"backend"** was used both for the version control system driving a
  repository and, loosely, for "the server side". Resolved: it means the version
  control system, only. A hosting service such as GitHub is a **Forge**, which is
  keyed off a repository's remote rather than its layout on disk.

- **"theme" vs "scheme"** were used interchangeably. Resolved: a **Scheme** is
  the named choice a user makes; the theme is the palette that choice resolves
  to.
