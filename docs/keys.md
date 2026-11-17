# Tab picker keys

Every key the tab picker (cmd+i) takes, by view and pane. `?` (F1 where
`?` is text) shows the keys for wherever the cursor is; these tables show
them all side by side, so a new binding can be checked against the rest.

`—` means the key does nothing there. Letters without ctrl search the list
in every list view.

## Ctrl keys

| Key       | Inbox list                          | Reply box                          | Work list                                | Archive                       | Usage  | Scrollback (⌃V)  | Work items float (⌃B)  |
| --------- | ----------------------------------- | ---------------------------------- | ---------------------------------------- | ----------------------------- | ------ | ---------------- | ---------------------- |
| ⌃A        | Archive view                        | —                                  | Archive view                             | back to Inbox                 | Archive view | —          | —                      |
| ⌃B        | work items float                    | —                                  | open the item in the browser             | work items float              | —      | page up          | close the float        |
| ⌃C        | arm an interrupt                    | clear, then leave                  | —                                        | —                             | —      | leave            | close the float        |
| ⌃D        | scrollback, half a page down        | leave for scrollback, half a page down | scrollback on the item, half a page down | half a page down          | —      | half a page down | reader: half a page down |
| ⌃E        | expand the preview / hide the sidebar | —                                | give the item the whole view             | —                             | —      | —                | —                      |
| ⌃F        | —                                   | —                                  | work items ↔ pull requests               | hide or show archived tasks   | —      | page down        | —                      |
| ⌃G        | —                                   | edit in the editor                 | arm a pull request's approval            | —                             | —      | —                | —                      |
| ⌃J ⌃K     | next, previous (wraps)              | leave for the list, next, previous | next, previous                           | next, previous (wraps)        | scroll | —                | —                      |
| ⌃L        | transcript → diff → commits         | —                                  | pull request: description → diff → commits | —                           | —      | —                | —                      |
| ⌃N ⌃P     | next, previous                      | —                                  | next, previous                           | next, previous                | scroll | —                | next, previous; reader: a line |
| ⌃O        | profile filter                      | —                                  | profile filter                           | profile filter                | profile filter | —        | —                      |
| ⌃R        | —                                   | —                                  | list the project again                   | —                             | —      | —                | —                      |
| ⌃S        | snooze or wake                      | —                                  | —                                        | —                             | —      | —                | —                      |
| ⌃T        | pin or unpin                        | —                                  | —                                        | —                             | —      | —                | —                      |
| ⌃U        | scrollback, half a page up          | leave for scrollback, half a page up | scrollback on the item, half a page up | half a page up                | clear the search | half a page up | reader: half a page up |
| ⌃V        | scrollback                          | scrollback, then back here         | scrollback on the item                   | —                             | —      | leave            | —                      |
| ⌃W        | delete a word of the search         | delete a word                      | delete a word of the search              | delete a word of the search   | delete a word | —         | —                      |
| ⌃X        | arm an archive                      | leave and arm an archive           | —                                        | arm an archive or worktree removal | — | —              | —                      |

Free everywhere: ⌃H, ⌃Y, ⌃Q, ⌃Z. ⌃I and ⌃M arrive as Tab and Enter.

## Other keys

| Key          | Inbox list                         | Reply box                     | Work list                                        | Archive          | Scrollback                         | Work items float       |
| ------------ | ---------------------------------- | ----------------------------- | ------------------------------------------------ | ---------------- | ---------------------------------- | ---------------------- |
| ⏎            | go to the tab, or resume           | send                          | start a session, or go to its task; approve if armed | resume       | copy and leave                     | open; confirm          |
| ⇥            | into the reply box                 | leave (while empty); take the suggestion | choose the project                    | choose the project | leave for the reply box (Inbox) | —                      |
| esc          | —                                  | leave                         | —                                                | —                | drop selection, search, then leave | back; dismiss          |
| ↑↓           | move                               | earlier messages              | move                                             | move             | a line                             | move; reader: a line   |
| ⇧↑↓ ⇧PgUp    | scrollback on the transcript       | scroll the transcript         | scroll the item beside the list                  | —                | —                                  | —                      |
| ⌥↑↓          | previous, next group               | —                             | —                                                | —                | —                                  | —                      |
| PgUp PgDn    | page                               | scroll the transcript         | page                                             | page             | page                               | reader: page           |
| Home End     | first, last                        | —                             | first, last                                      | first, last      | top, bottom                        | reader: top, bottom    |
| ⌥⌫           | delete a word of the search        | delete a word                 | delete a word of the search                      | delete a word    | —                                  | —                      |
| ⌥⏎ ⇧⏎        | —                                  | new line                      | —                                                | —                | —                                  | new line in a comment  |
| ? F1         | these keys                         | —                             | these keys                                       | these keys       | search back (`?`)                  | these keys (`?`)       |

The views themselves are picked with cmd+1-4 on the picker's tab: Inbox,
Work, Usage, Archive.

Scrollback adds vi-like keys of its own: `j` `k`, `{` `}`, `[` `]`, `g` `G`,
`/` `?` `n` `N`, `v` `V` Space and `y`. The work items float adds `o`, `v`,
`c`, `x`, `r` and, in its reader, `j` `k` Space `b` `g` `G` `q`. The help
float filters with `/` and clears the filter with ⌃U.
