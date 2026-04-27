# Terminal Polish Backlog

This audit ranks the remaining general terminal quality gaps for everyday shell
work. Existing strengths:

- Copy/paste actions exist and bracketed paste is tracked.
- Mouse input and terminal mouse modes are parsed.
- `capture` and `scrollback --tail` expose long output to automation.
- `ScreenBuffer` retains scrollback and supports anchor capture.

The remaining gaps are mostly interactive UI affordances.

## P1 Search

WTD has no in-pane find UI or actions equivalent to Windows Terminal `find` and
`findMatch`.

Follow-up:

- Add in-pane find/search UI and next/previous match actions.
- Search should operate across visible rows plus retained scrollback.
- Matches should be visible in scrollback mode and not interfere with terminal
  selection.

## P1 Scrollback Navigation

WTD has `enter-scrollback-mode`, and automation can use `scrollback --tail`, but
there are no explicit line/page/top/bottom scroll actions for normal operator
use.

Follow-up:

- Add scrollback line/page/top/bottom actions.
- Bind them in tmux and Windows Terminal presets where compatible.
- Ensure alternate-screen panes do not pollute primary scrollback navigation.

## P1 Keyboard Selection

Mouse selection and extraction tests exist, but WTD lacks keyboard mark mode,
select-all, and endpoint switching.

Follow-up:

- Add mark mode and select-all actions.
- Support keyboard expansion by character, word, line, and viewport.
- Keep selection state stable through scrollback movement and pane resize.

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
