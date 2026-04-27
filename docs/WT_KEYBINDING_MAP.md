# Windows Terminal Default Keybinding Map

Reference document for implementing the `windows-terminal` keybinding preset.

**Sources:**
- WT actions/defaults: `src/cascadia/TerminalSettingsModel/defaults.json` (microsoft/terminal, main branch)
- WT documentation: https://learn.microsoft.com/en-us/windows/terminal/customize-settings/actions
- WTD action catalog: `crates/wtd-host/src/action.rs` (`v1_registry`, §20.3)
- WTD default bindings: `crates/wtd-core/src/global_settings.rs` (`default_bindings`, §11.3)

---

## Key Spec Translation

WT and WTD use different casing conventions for key specs:

| Aspect | WT format | WTD format | Example |
|--------|-----------|------------|---------|
| Modifiers | lowercase | Title Case | `ctrl+shift+t` → `Ctrl+Shift+T` |
| Letter keys | lowercase | Uppercase | `t` → `T` |
| Function keys | lowercase | Uppercase | `f11` → `F11` |
| Arrow keys | `up/down/left/right` | `Up/Down/Left/Right` | `alt+down` → `Alt+Down` |
| Page keys | `pgup/pgdn` or `pageup/pagedown` | `PageUp/PageDown` | `ctrl+shift+pgup` → `Ctrl+Shift+PageUp` |
| Minus | `minus` | `Minus` | `alt+shift+-` or `alt+shift+minus` → `Alt+Shift+Minus` |
| Plus | `plus` | `Plus` | `ctrl+plus` → `Ctrl+Plus` |
| Numpad | `numpad_plus`, `numpad0`… | (not supported) | No WTD equivalent |
| Special | `sc(41)` (scan code) | (not supported) | No WTD equivalent |
| Insert | `insert` | `Insert` | `ctrl+insert` → `Ctrl+Insert` |

---

## Complete Mapping Table

Columns: WT action command / WT default key(s) / WTD action / WTD default key spec / status.

Status codes: `=` exact match, `~` partial/semantic match, `→` WT key translates to WTD with different default, `✗` no equivalent.

### Application / Window Actions

| WT Command | WT Default Keys | WTD Action | WTD Default Key | Status | Notes |
|---|---|---|---|---|---|
| `closeWindow` | `alt+f4` | `close-window` | (none) | `~` | WTD action exists; no default binding |
| `toggleFullscreen` | `alt+enter`, `f11` | `toggle-fullscreen` | `F11` | `=` | WTD binds F11 only; alt+enter unbound |
| `toggleFocusMode` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `toggleAlwaysOnTop` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `quit` | (none) | (none) | (none) | `✗` | No WTD equivalent (host has no quit-all) |
| `openSystemMenu` | `alt+space` | (none) | (none) | `✗` | OS-level; not applicable to WTD |
| `restoreLastClosed` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `identifyWindow` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `openWindowRenamer` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `quakeMode` | `win+sc(41)` | (none) | (none) | `✗` | Quake/summon mode not in WTD v1 |

### Command Palette / Settings

| WT Command | WT Default Keys | WTD Action | WTD Default Key | Status | Notes |
|---|---|---|---|---|---|
| `commandPalette` | `ctrl+shift+p` | `toggle-command-palette` | `Ctrl+Shift+Space` | `→` | Key differs: WT uses P, WTD uses Space |
| `openNewTabDropdown` | `ctrl+shift+space` | (none) | (none) | `✗` | WT-specific UI element; WTD has no profile picker dropdown |
| `openSettings (settingsUI)` | `ctrl+,` | (none) | (none) | `✗` | No WTD settings UI yet |
| `openSettings (settingsFile)` | `ctrl+shift+,` | (none) | (none) | `✗` | WTD uses YAML, no in-app editor |
| `openSettings (defaultsFile)` | `ctrl+alt+,` | (none) | (none) | `✗` | No WTD equivalent |

### Search / Find

| WT Command | WT Default Keys | WTD Action | WTD Default Key | Status | Notes |
|---|---|---|---|---|---|
| `find` | `ctrl+shift+f` | `find` | `Ctrl+Shift+F` | `=` | Prompts for in-pane search |
| `findMatch (next)` | (none) | `find-next` | `F3` | `~` | WTD adds a default next-match key |
| `findMatch (prev)` | (none) | `find-prev` | (none) | `~` | Available in command palette and tmux preset |
| `searchWeb` | (none) | (none) | (none) | `✗` | No WTD equivalent |

### Tab Actions

| WT Command | WT Default Keys | WTD Action | WTD Default Key | Status | Notes |
|---|---|---|---|---|---|
| `newTab` | `ctrl+shift+t` | `new-tab` | `Ctrl+Shift+T` | `=` | Exact match |
| `newWindow` | `ctrl+shift+n` | `new-window` | (none) | `~` | WTD action exists; no default binding |
| `newTab {index: 0..7}` | `ctrl+shift+1`..`ctrl+shift+8` | `new-tab` (no profile select by index) | (none) | `✗` | WT opens profile by index; WTD has no profile-index binding |
| `duplicateTab` | `ctrl+shift+d` | (none) | (none) | `✗` | No WTD duplicate-tab action |
| `nextTab` | `ctrl+tab` | `next-tab` | `Ctrl+Tab` | `=` | Exact match |
| `prevTab` | `ctrl+shift+tab` | `prev-tab` | `Ctrl+Shift+Tab` | `=` | Exact match |
| `switchToTab {index: 0..7}` | `ctrl+alt+1`..`ctrl+alt+8` | `goto-tab {index}` | (none) | `~` | WTD action exists; no default binding |
| `switchToTab (last)` | `ctrl+alt+9` | `goto-tab {name: "last"}` | (none) | `✗` | WTD goto-tab accepts name, not "last" |
| `closePane` | `ctrl+shift+w` | `close-pane` | `Ctrl+Shift+W` | `=` | Exact match (WT conflates tab/pane close) |
| `moveTab (forward)` | (none) | `move-tab-right` | (none) | `~` | WTD action exists; no default binding |
| `moveTab (backward)` | (none) | `move-tab-left` | (none) | `~` | WTD action exists; no default binding |
| `moveTabToNewWindow` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `renameTab` | (none) | `rename-tab` | (none) | `~` | WTD action exists; no default binding |
| `openTabRenamer` | (none) | `rename-tab` | (none) | `~` | WT has modal UI; WTD uses IPC |
| `openTabColorPicker` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `closeOtherTabs` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `closeTabsAfter` | (none) | (none) | (none) | `✗` | No WTD equivalent |

### Pane Actions

| WT Command | WT Default Keys | WTD Action | WTD Default Key | Status | Notes |
|---|---|---|---|---|---|
| `duplicatePaneRight` (`splitPane {split: right}`) | `alt+shift+plus` | `split-right` | `Alt+Shift+D` | `→` | Key differs: WT uses Alt+Shift+Plus, WTD uses Alt+Shift+D |
| `duplicatePaneDown` (`splitPane {split: down}`) | `alt+shift+-` | `split-down` | `Alt+Shift+Minus` | `=` | Same key (minus) |
| `splitPane (auto)` | (none) | (none) | (none) | `✗` | WTD has no auto-split direction |
| `closePane` | `ctrl+shift+w` | `close-pane` | `Ctrl+Shift+W` | `=` | Same binding |
| `closeOtherPanes` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `moveFocus (down)` | `alt+down` | `focus-pane-down` | (none) | `→` | WTD has no single-stroke default; chord: `Ctrl+B, Down` |
| `moveFocus (left)` | `alt+left` | `focus-pane-left` | (none) | `→` | WTD chord: `Ctrl+B, Left` |
| `moveFocus (right)` | `alt+right` | `focus-pane-right` | (none) | `→` | WTD chord: `Ctrl+B, Right` |
| `moveFocus (up)` | `alt+up` | `focus-pane-up` | (none) | `→` | WTD chord: `Ctrl+B, Up` |
| `moveFocus (nextInOrder)` | (none) | `focus-next-pane` | (none) | `~` | WTD chord: `Ctrl+B, o` |
| `moveFocus (prevInOrder)` | `ctrl+alt+left` | `focus-prev-pane` | (none) | `→` | WTD has action, no default binding |
| `togglePaneZoom` | (none) | `zoom-pane` | (none) | `~` | WTD chord: `Ctrl+B, z` |
| `resizePane (down)` | `alt+shift+down` | `resize-pane-down` | (none) | `→` | Move the nearest horizontal splitter down |
| `resizePane (up)` | `alt+shift+up` | `resize-pane-up` | (none) | `→` | Move the nearest horizontal splitter up |
| `resizePane (right)` | `alt+shift+right` | `resize-pane-right` | (none) | `→` | Move the nearest vertical splitter right |
| `resizePane (left)` | `alt+shift+left` | `resize-pane-left` | (none) | `→` | Move the nearest vertical splitter left |
| `swapPane (direction)` | (none) | `swap-pane-{dir}` | (none) | `~` | Available in WTD command palette; no default binding yet |
| `toggleSplitOrientation` | (none) | `toggle-split-orientation` | (none) | `~` | Available in WTD command palette; no default binding yet |
| `toggleBroadcastInput` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `movePane (index)` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `movePaneToNewWindow` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `restartConnection` | (none) | `restart-session` | (none) | `~` | WTD chord: `Ctrl+B, x` (close-pane, not restart) |
| `toggleReadOnlyMode` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `enableReadOnlyMode` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `disableReadOnlyMode` | (none) | (none) | (none) | `✗` | No WTD equivalent |

### Clipboard / Selection

| WT Command | WT Default Keys | WTD Action | WTD Default Key | Status | Notes |
|---|---|---|---|---|---|
| `copy` | `ctrl+shift+c`, `ctrl+insert` | `copy` | `Ctrl+Shift+C`, `Ctrl+Insert` | `=` | Primary and secondary WT bindings |
| `paste` | `ctrl+shift+v`, `shift+insert` | `paste` | `Ctrl+Shift+V`, `Shift+Insert` | `=` | Primary and secondary WT bindings |
| `selectAll` | `ctrl+shift+a` | `select-all` | `Ctrl+Shift+A` | `=` | Selects the focused pane viewport |
| `markMode` | `ctrl+shift+m` | `mark-mode` | `Ctrl+Shift+M` | `=` | Enters keyboard selection mode |
| `toggleBlockSelection` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `switchSelectionEndpoint` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `expandSelectionToWord` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `showContextMenu` | `menu` | (none) | (none) | `✗` | Right-click context menu; no WTD equivalent |

### Scrollback / Buffer

| WT Command | WT Default Keys | WTD Action | WTD Default Key | Status | Notes |
|---|---|---|---|---|---|
| `scrollDown` | `ctrl+shift+down` | (none) | (none) | `✗` | WTD enters scrollback mode but has no scroll actions |
| `scrollDownPage` | `ctrl+shift+pgdn` | (none) | (none) | `✗` | No WTD equivalent |
| `scrollUp` | `ctrl+shift+up` | (none) | (none) | `✗` | No WTD equivalent |
| `scrollUpPage` | `ctrl+shift+pgup` | (none) | (none) | `✗` | No WTD equivalent |
| `scrollToTop` | `ctrl+shift+home` | (none) | (none) | `✗` | No WTD equivalent |
| `scrollToBottom` | `ctrl+shift+end` | (none) | (none) | `✗` | No WTD equivalent |
| `clearBuffer` | `ctrl+shift+k` | (none) | (none) | `✗` | No WTD equivalent |
| `exportBuffer` | (none) | (none) | (none) | `✗` | No WTD equivalent |

### Font / View

| WT Command | WT Default Keys | WTD Action | WTD Default Key | Status | Notes |
|---|---|---|---|---|---|
| `adjustFontSize {delta: 1}` | `ctrl+plus`, `ctrl+numpad_plus` | (none) | (none) | `✗` | No WTD font size action |
| `adjustFontSize {delta: -1}` | `ctrl+minus`, `ctrl+numpad_minus` | (none) | (none) | `✗` | No WTD font size action |
| `resetFontSize` | `ctrl+0`, `ctrl+numpad_0` | (none) | (none) | `✗` | No WTD font size action |
| `adjustOpacity` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `toggleShaderEffects` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `setColorScheme` | (none) | (none) | (none) | `✗` | No WTD equivalent |

### Suggestions / Tasks (Newer WT Features)

| WT Command | WT Default Keys | WTD Action | WTD Default Key | Status | Notes |
|---|---|---|---|---|---|
| `showSuggestions` | `ctrl+shift+.` | (none) | (none) | `✗` | No WTD equivalent |
| `quickFix` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `experimental.openTasks` | (none) | (none) | (none) | `✗` | Experimental; no WTD equivalent |
| `openCWD` | (none) | (none) | (none) | `✗` | No WTD equivalent |
| `openAbout` | (none) | (none) | (none) | `✗` | No WTD equivalent |

---

## Gap Analysis

### WT Actions With No WTD Equivalent (WTD gaps)

This audit tracks WT action coverage gaps and recent parity additions. Missing
actions must be omitted from the WT preset until a future bead adds the matching
WTD action.

**UI / Window features** (low priority for terminal-mode WTD):
- `toggleFocusMode` — Hide tabs/title bar
- `toggleAlwaysOnTop` — Always-on-top window
- `openNewTabDropdown` — Profile picker dropdown (WTD uses command palette instead)
- `openSettings (all)` — Settings UI / file editor
- `identifyWindow` / `openWindowRenamer` — Window naming UI
- `quakeMode` — Summon-from-tray
- `restoreLastClosed` — Restore last closed
- `quit` — Close all windows
- `openSystemMenu` — OS system menu
- `showContextMenu` — Right-click menu
- `openAbout` — About dialog
- `openCWD` — Open CWD in Explorer

**Search** (implemented):
- `find` — In-pane text search (Ctrl+Shift+F)
- `findMatch (next/prev)` — `find-next` / `find-prev`

**Selection / Clipboard** (partial):
- `selectAll` — `select-all` (Ctrl+Shift+A)
- `markMode` — `mark-mode` (Ctrl+Shift+M)
- `switchSelectionEndpoint` — `switch-selection-endpoint` (command palette and tmux prefix+`s`)
- `toggleBlockSelection` / `expandSelectionToWord` — Selection primitives exist for block, line, and word expansion; direct WT parity bindings are still omitted.

**Scrollback** (line/page/top/bottom implemented as inline focused-pane navigation):
- `scrollUp` / `scrollDown` — `scrollback-line-up` / `scrollback-line-down` (Ctrl+Shift+Up/Down)
- `scrollUpPage` / `scrollDownPage` — `scrollback-page-up` / `scrollback-page-down` (Ctrl+Shift+PgUp/PgDn)
- `scrollToTop` / `scrollToBottom` — `scrollback-top` / `scrollback-bottom` (Ctrl+Shift+Home/End)
- `clearBuffer` — Clear scrollback (Ctrl+Shift+K; still missing)

**Font size** (should be added in a future action bead):
- `adjustFontSize {delta: ±1}` — Zoom in/out (Ctrl+Plus / Ctrl+Minus)
- `resetFontSize` — Reset zoom (Ctrl+0)

**Pane operations** (future beads):
- `toggleBroadcastInput` — Input broadcasting
- `movePane` / `movePaneToNewWindow` — Move pane between tabs/windows
- `closeOtherPanes` — Close all but focused

**Implemented without default bindings**:
- `swap-pane-up` / `swap-pane-down` / `swap-pane-left` / `swap-pane-right`
- `toggle-split-orientation`
- `equalize-pane-split` / `equalize-tab`
- `retile-even-horizontal` / `retile-even-vertical` / `retile-grid`
- `retile-main-left` / `retile-main-right` / `retile-main-top` / `retile-main-bottom`

**Tab operations** (future beads):
- `duplicateTab` — Duplicate tab (Ctrl+Shift+D)
- `closeOtherTabs` / `closeTabsAfter` — Bulk tab close
- `moveTabToNewWindow` — Move tab to new window

**Read-only mode** (future bead):
- `toggleReadOnlyMode` / `enableReadOnlyMode` / `disableReadOnlyMode`

**Newer WT features** (not planned):
- `showSuggestions` — Shell suggestions
- `quickFix` — Quick fix
- `experimental.openTasks`
- `searchWeb` — Web search
- `exportBuffer` — Export to file
- `adjustOpacity` / `toggleShaderEffects` / `setColorScheme`

### WTD Actions With No WT Equivalent (WTD-unique actions)

These WTD actions exist in the v1 catalog but have no WT counterpart. Users migrating from WT will need to discover these:

| WTD Action | Description | WTD Default | Notes |
|---|---|---|---|
| `open-workspace` | Open a named workspace | (none) | WTD-specific concept |
| `close-workspace` | Close workspace (detach or kill) | `Ctrl+B, d` | WTD-specific concept |
| `recreate-workspace` | Tear down and rebuild from YAML | (none) | WTD-specific concept |
| `save-workspace` | Persist current layout to YAML | (none) | WTD-specific concept |
| `focus-pane` (by name) | Jump to a named pane | (none) | WT has no named panes |
| `rename-pane` | Rename a pane | `Ctrl+B, ,` | WT has no named panes |
| `focus-prev-pane` | Focus previous pane (cycle backward) | (none) | WT has no `prevInOrder` default binding |
| `enter-scrollback-mode` | Enter modal scrollback navigation | `Ctrl+B, [` | WT scrolls inline, no mode switch |
| `restart-session` | Kill and restart session from same definition | (none) | WT's `restartConnection` is analogous |

---

## Proposed `windows-terminal` Preset Bindings

This is the recommended binding set for a `windows-terminal` preset (bead wintermdriver-h35.3). It maps WT default keys to WTD actions where possible, omits WT-only actions, and preserves WTD-unique actions with WTD defaults.

### Single-stroke bindings

| Key Spec | WTD Action | WT Source | Notes |
|---|---|---|---|
| `Ctrl+Shift+T` | `new-tab` | `ctrl+shift+t` | Exact match |
| `Ctrl+Shift+W` | `close-pane` | `ctrl+shift+w` | Exact match |
| `Ctrl+Shift+C` | `copy` | `ctrl+shift+c` | Exact match |
| `Ctrl+Shift+V` | `paste` | `ctrl+shift+v` | Exact match |
| `Ctrl+Tab` | `next-tab` | `ctrl+tab` | Exact match |
| `Ctrl+Shift+Tab` | `prev-tab` | `ctrl+shift+tab` | Exact match |
| `Ctrl+Shift+P` | `toggle-command-palette` | `ctrl+shift+p` | WT uses P; WTD default uses Space |
| `F11` | `toggle-fullscreen` | `f11` | Exact match |
| `Alt+Shift+Plus` | `split-right` | `alt+shift+plus` | WT uses Plus; WTD default uses D |
| `Alt+Shift+Minus` | `split-down` | `alt+shift+-` | Exact match |
| `Alt+Down` | `focus-pane-down` | `alt+down` | WT uses single-stroke; WTD default uses chord |
| `Alt+Up` | `focus-pane-up` | `alt+up` | WT uses single-stroke; WTD default uses chord |
| `Alt+Left` | `focus-pane-left` | `alt+left` | WT uses single-stroke; WTD default uses chord |
| `Alt+Right` | `focus-pane-right` | `alt+right` | WT uses single-stroke; WTD default uses chord |
| `Alt+Shift+Down` | `resize-pane-down` | `alt+shift+down` | Moves the nearest horizontal splitter down, matching WT |
| `Alt+Shift+Up` | `resize-pane-up` | `alt+shift+up` | Moves the nearest horizontal splitter up, matching WT |
| `Alt+Shift+Right` | `resize-pane-right` | `alt+shift+right` | Moves the nearest vertical splitter right, matching WT |
| `Alt+Shift+Left` | `resize-pane-left` | `alt+shift+left` | Moves the nearest vertical splitter left, matching WT |
| `Alt+Shift+K` | `pass-through-next-key` | WTD-only | Arms one-shot key pass-through for the focused pane |
| `Ctrl+Insert` | `copy` | `ctrl+insert` | Secondary WT binding |
| `Shift+Insert` | `paste` | `shift+insert` | Secondary WT binding |
| `Ctrl+Shift+Up` | `scrollback-line-up` | `ctrl+shift+up` | Scrolls the focused pane back one line; ignored on alternate screen |
| `Ctrl+Shift+Down` | `scrollback-line-down` | `ctrl+shift+down` | Scrolls the focused pane toward live output one line |
| `Ctrl+Shift+PageUp` | `scrollback-page-up` | `ctrl+shift+pgup` | Scrolls the focused pane back one page |
| `Ctrl+Shift+PageDown` | `scrollback-page-down` | `ctrl+shift+pgdn` | Scrolls the focused pane toward live output one page |
| `Ctrl+Shift+Home` | `scrollback-top` | `ctrl+shift+home` | Jumps to the oldest retained scrollback row |
| `Ctrl+Shift+End` | `scrollback-bottom` | `ctrl+shift+end` | Jumps back to live output |

### Notes on omitted WT bindings in the preset

- `ctrl+shift+space` (openNewTabDropdown) — omitted; WT UI element with no WTD equivalent
- `ctrl+,` / `ctrl+shift+,` / `ctrl+alt+,` (openSettings) — omitted; no WTD settings UI
- `ctrl+shift+n` (newWindow) — omitted; WTD action exists but opening a new window requires workspace context
- `ctrl+shift+d` (duplicateTab) — omitted; no WTD equivalent
- `ctrl+alt+1`..`9` (switchToTab) — omitted; WTD has goto-tab but profile-index launch not supported
- Font size keys (Ctrl+Plus/Minus/0) — omitted; no WTD action
- `ctrl+shift+k` (clearBuffer) — omitted; no WTD action
- `alt+f4` (closeWindow) — omitted; OS-level; `close-window` requires workspace context
- `alt+enter` (toggleFullscreen) — omitted as secondary; F11 sufficient

---

## Key Spec Issues and Gotchas

1. **`alt+shift+plus` in WT**: The `+` in WT means the plus key, not the Shift modifier. WTD's key spec for this would be `Alt+Shift+Plus`. This is the same physical key as `=` / `+` on a US keyboard (with Shift held). In WTD's `KeyName` enum this is `Plus`.

2. **Numpad keys**: WT defines `ctrl+numpad_plus`, `ctrl+numpad_minus`, `ctrl+numpad_0` as secondary bindings for font size. WTD's `KeyName` enum does not define numpad keys — these cannot be represented in WTD key specs without a new `KeyName` variant.

3. **Scan codes**: WT's `win+sc(41)` for quake mode uses a raw keyboard scan code. WTD has no scan-code support.

4. **`menu` key**: WT binds `showContextMenu` to the `menu`/`app` key. WTD's `KeyName` enum does not include this key.

5. **`ctrl+alt+left` (moveFocus prevInOrder)**: This is a standard WT binding that would conflict on many systems with the Win10/11 virtual desktop switching shortcut (`Ctrl+Alt+Left/Right`). The WT preset should document this conflict.

6. **`ctrl+shift+space`**: WT binds this to `openNewTabDropdown`. WTD binds it to `toggle-command-palette`. If a user installs the WT preset, this key would need to be reassigned.

7. **Case sensitivity**: WTD's `KeySpec::parse()` is case-insensitive for modifier names but requires letters in any case (normalized to uppercase internally). WT users writing `ctrl+shift+t` in WTD config will need to be aware of the casing convention.
