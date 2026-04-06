# UI Test YAML

This file describes the current Orbit UI flow grammar that is accepted by the
parser. Parser support is broader than backend support, so also read
[ui-test-platforms.md](ui-test-platforms.md) before using a command in a real
flow.

## Supported Document Shapes

### 1. Single command list

```yaml
- launchApp
- assertVisible: Ready
```

### 2. Two YAML documents

```yaml
appId: dev.orbit.fixture
name: Login
---
- launchApp
- assertVisible: Ready
```

### 3. Single document with `steps`

```yaml
appId: dev.orbit.fixture
name: Login
steps:
  - launchApp
  - assertVisible: Ready
```

Supported top-level config keys are:

- `appId`
- `name`
- `steps`

Unknown config keys are rejected.

## Selectors

Selectors are either:

- a string, which means text match
- a mapping with `text`, `id`, or both

Examples:

```yaml
- tapOn: Continue
- assertVisible:
    id: login-button
- assertVisible:
    text: Ready
    id: status-label
```

## Durations, Coordinates, And Scalars

Durations can be:

- integers in milliseconds
- strings like `750ms`
- strings like `2s`

Point expressions are strings in `x, y` form. Each coordinate can be:

- absolute, for example `140`
- percent, for example `90%`

Examples:

```yaml
- tapOnPoint: 140, 142
- swipe:
    start: 90%, 50%
    end: 10%, 50%
    duration: 800ms
```

## App Lifecycle Commands

### `launchApp`

Supported forms:

```yaml
- launchApp
- launchApp: dev.orbit.fixture
- launchApp:
    appId: dev.orbit.fixture
    clearState: true
    clearKeychain: true
    stopApp: false
    arguments:
      seededEmail: qa@example.com
      onboardingComplete: true
    permissions:
      location: allow
      photos: deny
```

Defaults:

- `stopApp: true`
- `clearState: false`
- `clearKeychain: false`

`launchApp.arguments` must be a mapping of scalar values.

### `stopApp`, `killApp`, `clearState`

Supported forms:

```yaml
- stopApp
- killApp
- clearState
- stopApp: dev.orbit.fixture
- clearState:
    appId: dev.orbit.fixture
```

### `clearKeychain`

Bare command only:

```yaml
- clearKeychain
```

## Interaction Commands

### Taps And Clicks

```yaml
- tapOn: Continue
- doubleTapOn:
    id: hero-card
- hoverOn:
    id: hover-target
- rightClickOn:
    id: context-target
- longPressOn:
    element: Continue
    duration: 1200ms
- tapOnPoint: 140, 142
```

### Swipes, Scrolls, And Drag

`swipe` accepts either a direction or explicit start/end points:

```yaml
- swipe: LEFT
- swipe:
    direction: LEFT
    duration: 650ms
    delta: 4
- swipe:
    start: 90%, 50%
    end: 10%, 50%
    duration: 800ms
    delta: 5
```

Element-scoped gesture forms:

```yaml
- swipeOn:
    element:
      id: pager
    direction: LEFT
    duration: 650ms
    delta: 4
- scroll: DOWN
- scrollOn:
    element:
      id: feed
    direction: UP
- scrollUntilVisible:
    element:
      text: Ready
    direction: DOWN
    timeout: 3s
- dragAndDrop:
    from:
      id: drag-source
    to:
      id: drop-target
    duration: 800ms
    delta: 3
```

Notes:

- `scrollOn.direction` defaults to `DOWN`
- `scrollUntilVisible.direction` defaults to `DOWN`
- `scrollUntilVisible.timeout` defaults to `20s`
- `dragAndDrop` also accepts `source` and `destination` aliases

## Text And Keyboard Commands

```yaml
- inputText: hello
- pasteText
- pasteText: {}
- setClipboard: copied value
- copyTextFrom:
    id: email-value
- eraseText
- eraseText: 6
- eraseText:
    characters: 12
- pressKey: ENTER
- pressKey:
    key: K
    modifiers:
      - COMMAND
      - SHIFT
- pressKeyCode: 41
- pressKeyCode:
    keyCode: 41
    duration: 200ms
    modifiers: CONTROL
- keySequence:
    - 4
    - 5
    - 6
- pressButton: SIRI
- pressButton:
    button: SIRI
    duration: 500ms
- selectMenuItem: Automation > Trigger Shortcut
- selectMenuItem:
    path:
      - Automation
      - Trigger Shortcut
- hideKeyboard
```

Defaults:

- bare `eraseText` deletes `50` characters

Supported modifier names:

- `COMMAND` or `CMD`
- `SHIFT`
- `OPTION` or `ALT`
- `CONTROL` or `CTRL`
- `FUNCTION` or `FN`

Supported `pressButton` values:

- `APPLE_PAY`
- `HOME`
- `LOCK`
- `SIDE_BUTTON`
- `SIRI`

## Assertions And Waits

```yaml
- assertVisible: Ready
- assertNotVisible:
    id: spinner
- extendedWaitUntil:
    visible:
      text: Ready
    timeout: 2s
- extendedWaitUntil:
    notVisible:
      id: spinner
    timeout: 10s
- waitForAnimationToEnd
- waitForAnimationToEnd: 750ms
- waitForAnimationToEnd:
    timeout: 750ms
```

Defaults:

- `extendedWaitUntil.timeout` defaults to `10s`
- bare `waitForAnimationToEnd` defaults to `5s`

## Artifacts And Environment

```yaml
- takeScreenshot: login-ready
- takeScreenshot:
    name: login-ready
- startRecording: login-clip
- startRecording:
    path: login-clip
- stopRecording
- openLink: https://example.com
- setLocation:
    latitude: 55.7558
    longitude: 37.6173
- setPermissions:
    permissions:
      reminders: unset
- setPermissions:
    appId: dev.orbit.fixture
    permissions:
      location: allow
      photos: deny
- travel:
    points:
      - 55.7558,37.6173
      - 55.7568,37.6183
    speed: 42
- addMedia:
    - ../Fixtures/cat.jpg
```

Notes:

- `takeScreenshot` and `startRecording` accept a string, `null`, or a mapping with `name` or `path`
- `stopRecording` does not take an inline value
- `travel.points` must contain at least two coordinates
- permission states are `allow`, `deny`, or `unset`

## Flow Composition

```yaml
- runFlow: shared/login.yaml
- repeat:
    times: 3
    commands:
      - tapOn: Retry
- retry:
    times: 2
    commands:
      - assertVisible: Welcome
```

Rules:

- `repeat` and `retry` require a mapping with `times` and `commands`
- `retry.times` must be greater than zero
- `runFlow` recursion is rejected

## Common Parser Rejections

The parser rejects:

- unknown top-level config keys
- unknown command names
- command mappings with more than one key
- selectors that omit both `text` and `id`
- bad durations or negative integer fields
- `pasteText` or `stopRecording` with inline scalar values
- empty `keySequence`
- empty `permissions`
- `travel.points` with fewer than two coordinates
