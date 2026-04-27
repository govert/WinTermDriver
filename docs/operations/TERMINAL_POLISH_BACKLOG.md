# Terminal Polish Backlog

This audit ranks the remaining general terminal quality gaps for everyday shell
work. Existing strengths:

- Copy/paste actions exist and bracketed paste is tracked.
- Mouse input and terminal mouse modes are parsed.
- `capture` and `scrollback --tail` expose long output to automation.
- `ScreenBuffer` retains scrollback and supports anchor capture.

The remaining gaps are mostly interactive UI affordances.

## P1 Search

Status: base in-pane search is addressed by `find`, `find-next`, and
`find-prev`.

Implemented:

- Ctrl+Shift+F opens a focused-pane search prompt.
- Matches are computed across visible rows plus retained scrollback.
- Next/previous navigation scrolls the viewport to the current match and
  highlights it with the selection overlay.

Remaining polish:

- Add distinct all-match highlighting separate from the selection overlay.
- Add richer search UI affordances such as match count and editable query state.

## P1 Scrollback Navigation

Status: addressed by `scrollback-line-up`, `scrollback-line-down`,
`scrollback-page-up`, `scrollback-page-down`, `scrollback-top`, and
`scrollback-bottom`.

Implemented:

- Windows Terminal preset binds Ctrl+Shift+Up/Down/PageUp/PageDown/Home/End.
- tmux preset binds prefix+PageUp/PageDown/Home/End and keeps prefix+`[` for
  modal scrollback entry.
- Alternate-screen panes ignore these local viewport actions.

## P1 Keyboard Selection

Status: base keyboard selection is addressed by `mark-mode`, `select-all`, and
`switch-selection-endpoint`.

Implemented:

- Windows Terminal preset binds Ctrl+Shift+M and Ctrl+Shift+A.
- Mark mode moves the active endpoint with arrow, Home/End, and PageUp/PageDown.
- `switch-selection-endpoint` changes which endpoint movement affects.

Remaining polish:

- Add word, line, and block expansion modes.
- Make selection anchoring smarter across scrollback movement and pane resize.

## P2 Mouse Selection Polish

Basic selection extraction exists, but advanced selection behaviors are not yet
ranked or exposed.

Follow-up:

- Add word selection, line selection, block selection, and selection endpoint
  switching.
- Document how app mouse-tracking mode interacts with terminal selection.
- Add tests for wide characters, combining marks, hyperlinks, and wrapped lines.

## P2 Buffer Management

WTD has retained scrollback but no user-facing clear-buffer action equivalent to
Windows Terminal `clearBuffer`.

Follow-up:

- Add clear scrollback/current-buffer actions with explicit scope.
- Ensure automation capture responses reflect the cleared buffer deterministically.

## P2 Secondary Keybinding Parity

The Windows Terminal compatibility docs show secondary bindings such as
`Ctrl+Insert` for copy and `Shift+Insert` for paste. Primary bindings exist, but
secondary parity is incomplete.

Follow-up:

- Add secondary copy/paste bindings where they do not conflict with app input.
- Document any intentional omissions.

## Ranked Follow-Up Beads

1. In-pane search and find-match navigation.
2. Scrollback navigation actions and keybindings.
3. Keyboard mark mode and select-all.
4. Mouse selection polish.
5. Clear buffer and scrollback actions.
6. Secondary copy/paste keybinding parity.
