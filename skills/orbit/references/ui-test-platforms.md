# UI Test Platforms

This file describes current backend support for Orbit UI tests. The YAML parser
accepts more commands than any single runtime backend supports.

## iOS Simulator

README currently documents support for these commands on the iOS simulator:

- `launchApp`
- `stopApp`
- `killApp`
- `clearState`
- `clearKeychain`
- `tapOn`
- `tapOnPoint`
- `doubleTapOn`
- `longPressOn`
- `swipe`
- `scroll`
- `scrollUntilVisible`
- `inputText`
- `pasteText`
- `setClipboard`
- `copyTextFrom`
- `eraseText`
- `pressKey`
- `pressKeyCode`
- `keySequence`
- `pressButton`
- `hideKeyboard`
- `assertVisible`
- `assertNotVisible`
- `extendedWaitUntil`
- `waitForAnimationToEnd`
- `takeScreenshot`
- `startRecording`
- `stopRecording`
- `openLink`
- `setLocation`
- `setPermissions`
- `travel`
- `addMedia`
- `runFlow`
- `repeat`
- `retry`

For simulator debugging outside the YAML flow language, Orbit also provides:

- `orbit ui dump-tree --platform ios`
- `orbit ui describe-point --platform ios --x <x> --y <y>`
- `orbit ui focus --platform ios`
- `orbit ui logs --platform ios -- ...`
- `orbit ui open --platform ios <url>`
- `orbit ui add-media --platform ios <path>`
- `orbit ui install-dylib --platform ios <path>`
- `orbit ui instruments --platform ios --template ...`
- `orbit ui update-contacts --platform ios <sqlite>`
- `orbit ui crash --platform ios ...`

## macOS

README currently documents macOS backend coverage for:

- `launchApp`
- `stopApp`
- `clearState`
- `tapOn`
- `hoverOn`
- `rightClickOn`
- `dragAndDrop`
- `swipe`
- `scroll`
- `scrollUntilVisible`
- `inputText`
- `assertVisible`
- `takeScreenshot`
- window-scoped video recording
- `openLink`

The current macOS backend also supports some keyboard interaction, but it is not
fully symmetric with iOS.

## macOS Restrictions From The Current Backend

The current macOS backend explicitly rejects:

- `pressButton`
- `clearKeychain`
- `setLocation`
- `setPermissions`
- `travel`
- `addMedia`

For `pressKey` on macOS, the backend only supports:

- `ENTER`
- `BACKSPACE`
- `ESCAPE` or `BACK`
- `SPACE`
- `TAB`
- `HOME`
- arrow keys
- printable characters that can be mapped to a macOS keycode

The current macOS backend rejects `LOCK`, `POWER`, `VOLUME_UP`, and
`VOLUME_DOWN` key presses.

The README also notes that modified keyboard shortcuts on macOS are not yet
documented as stable. Treat `COMMAND` and other modifier-heavy flows as
machine-dependent and verify them on the actual target machine.

## Authoring Guidance

- Start from commands that are clearly documented for the target platform.
- If a flow uses parser-supported commands that are not listed for the target
  backend, verify support before depending on them.
- Prefer `id` selectors over pure text when the app can expose stable IDs.
- Use Orbit helper commands such as `dump-tree` and `describe-point` to debug
  selectors before broad flow rewrites.
