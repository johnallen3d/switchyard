# Tab picker keys

Every key the tab picker (cmd+i) takes, by view and pane. `?` (F1 where
`?` is text) shows the keys for wherever the cursor is; these tables show
them all side by side, so a new binding can be checked against the rest.

`—` means the key does nothing there. Letters without ctrl search the list
in every list view.

## Ctrl keys

| Key       | Inbox list                          | Reply box                          | Work list                                | Archive                       | Search (⌃Q)                   | Usage  | Scrollback (⌃V)  | Work items float (⌃B)  |
| --------- | ----------------------------------- | ---------------------------------- | ---------------------------------------- | ----------------------------- | ----------------------------- | ------ | ---------------- | ---------------------- |
| ⌃A        | Archive view                        | —                                  | Archive view                             | back to Inbox                 | Archive view                  | Archive view | —          | —                      |
| ⌃B        | work items float                    | —                                  | open the item in the browser             | work items float              | —                             | —      | page up          | close the float        |
| ⌃C        | arm an interrupt                    | clear, then leave                  | —                                        | —                             | —                             | —      | leave            | close the float        |
| ⌃D        | scrollback, half a page down        | leave for scrollback, half a page down | scrollback on the item, half a page down | half a page down          | half a page down              | —      | half a page down | reader: half a page down |
| ⌃E        | switch harness float                | —                                  | —                                        | switch harness float          | —                             | —      | —                | —                      |
| ⌃F        | —                                   | —                                  | work items ↔ pull requests               | hide or show archived tasks   | harness filter (all/claude/pi) | —     | page down        | —                      |
| ⌃G        | —                                   | edit in the editor                 | arm a pull request's approval            | jump to first row             | —                             | —      | —                | —                      |
| ⌃H        | move host float                     | —                                  | —                                        | move host float               | —                             | —      | —                | —                      |
| ⌃J ⌃K     | next, previous (wraps)              | leave for the list, next, previous | next, previous                           | next, previous (wraps)        | next, previous                | scroll | —                | —                      |
| ⌃L        | transcript → diff → commits         | —                                  | pull request: description → diff → commits | —                           | —                             | —      | —                | —                      |
| ⌃N ⌃P     | next, previous                      | —                                  | next, previous                           | next, previous                | next, previous                | scroll | —                | next, previous; reader: a line |
| ⌃O        | profile filter                      | —                                  | profile filter                           | profile filter                | store filter (all/personal/work) | profile filter | —     | —                      |
| ⌃Q        | transcript search                   | —                                  | transcript search                        | transcript search             | back to previous view         | transcript search | —     | —                      |
| ⌃R        | pin or unpin                        | —                                  | list the project again                   | —                             | run the search again          | —      | —                | —                      |
| ⌃S        | expand the preview / hide the sidebar | expand / collapse sidebar        | give the item the whole view             | —                             | —                             | —      | collapse sidebar | —                      |
| ⌃T        | review in tuicr                     | —                                  | review a pull request in tuicr           | —                             | —                             | —      | —                | —                      |
| ⌃U        | scrollback, half a page up          | leave for scrollback, half a page up | scrollback on the item, half a page up | half a page up                | clear query (typing) / half page up | clear the search | half a page up | reader: half a page up |
| ⌃V        | scrollback                          | scrollback, then back here         | scrollback on the item                   | —                             | —                             | —      | leave            | —                      |
| ⌃W        | delete a word of the search         | delete a word                      | delete a word of the search              | delete a word of the search   | delete a word of the query    | delete a word | —         | —                      |
| ⌃X        | arm an archive                      | leave and arm an archive           | arm closing a card                       | arm an archive or worktree removal | —                        | —      | —                | —                      |
| ⌃Y        | copy host, session ID and log path  | —                                  | —                                        | —                             | copy store, session ID and cwd | —     | —                | —                      |
| ⌃Z        | snooze prompt                       | —                                  | arm postponing a card                    | —                             | —                             | —      | —                | —                      |

Free everywhere: none (⌃I and ⌃M arrive as Tab and Enter).

## Other keys

| Key          | Inbox list                         | Reply box                     | Work list                                        | Archive          | Search (⌃Q)                                         | Scrollback                         | Work items float       |
| ------------ | ---------------------------------- | ----------------------------- | ------------------------------------------------ | ---------------- | --------------------------------------------------- | ---------------------------------- | ---------------------- |
| ⏎            | clear search (⌘⏎: go or resume)    | send                          | start a session, or go to its task; approve or merge if armed | resume       | submit query (typing) / show task in Inbox or Archive (⌘⏎: go or resume) | copy and leave                     | open; confirm          |
| ⇥            | into the reply box                 | leave (while empty); take the suggestion | choose the project                    | choose the project | cycle harness filter (all/claude/pi)                | leave for the reply box (Inbox)    | —                      |
| esc          | leave the search                   | leave                         | leave the search                                 | leave the search | stop typing                                         | drop selection, search, then leave | back; dismiss          |
| ↑↓           | move                               | earlier messages              | move                                             | move             | move                                                | a line                             | move; reader: a line   |
| ⇧↑↓ ⇧PgUp    | scrollback on the transcript       | scroll the transcript         | scroll the item beside the list                  | —                | scroll the match detail beside the list             | —                                  | —                      |
| ⌥↑↓          | previous, next group               | —                             | —                                                | —                | —                                                   | —                                  | —                      |
| PgUp PgDn    | page                               | scroll the transcript         | page                                             | page             | page                                                | page                               | reader: page           |
| Home End     | first, last                        | —                             | first, last                                      | first, last      | first, last                                         | top, bottom                        | reader: top, bottom    |
| ⌥⌫           | delete a word of the search        | delete a word                 | delete a word of the search                      | delete a word    | delete a word of the query                          | —                                  | —                      |
| ⌥⏎ ⇧⏎        | —                                  | new line                      | —                                                | —                | —                                                   | —                                  | new line in a comment  |
| ? F1         | these keys                         | —                             | these keys                                       | these keys       | these keys (`F1` while typing)                      | search back (`?`)                  | these keys (`?`)       |

The views themselves are picked with cmd+1-5 on the picker's tab: Inbox,
Work, Usage, Archive, Search. In Search (outside typing), `r` jumps to a
live task's tab or resumes an ended task, `/` edits the query, and `g`/`G`
jump to the first or last result.

Scrollback adds vi-like keys of its own: `j` `k`, `{` `}`, `[` `]`, `g` `G`,
`/` `?` `n` `N`, `v` `V` Space and `y`. The work items float adds Enter (show
the item in the Work view), `o` (open it in the browser), `v`, `c`, `x`, `r`
and, in its reader, `j` `k` Space `b` `g` `G` `q`. The help
float filters with `/` and clears the filter with ⌃U.
