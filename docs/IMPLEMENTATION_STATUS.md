# tscode 개발 현황

이 문서는 tscode가 현재 어디까지 구현됐고, VS Code를 TUI로 옮기기 위해 무엇이 아직 남아 있는지 추적한다.

## 현재 릴리스 정책

제품이 완성 단계에 가까워지기 전까지 prerelease CI는 Apple Silicon macOS 타깃만 빌드한다.

- `aarch64-apple-darwin`

macOS x86_64, Linux, Windows, Raspberry Pi 전체 빌드 매트릭스는 반복 속도를 위해 일시 중단했다. 사용자가 다시 전체 OS 빌드를 지시하면 기존 멀티플랫폼 릴리스 흐름을 복구한다.

## 최근 안정화 작업

- 크래시 발생 시 터미널 raw mode, 마우스 캡처, 커서, alternate screen을 복구한다.
- 크래시 리포트는 `$XDG_CACHE_HOME/tscode/crash.log`, `~/.cache/tscode/crash.log`, 또는 임시 디렉터리 fallback에 저장한다.
- 크래시 로그는 기존 내용을 덮어쓰지 않고 append한다.
- 각 크래시 로그 항목은 UTC 날짜/시간 헤더, `time_utc`, `unix_time`, 버전, 발생 위치, panic 메시지, backtrace를 포함한다.
- 마우스 휠/hover 경로에서 짧은 코드 라인의 실제 길이보다 큰 column이 들어와도 식별자 계산이 panic하지 않도록 경계 처리를 수정했다.
- crossterm 공식 문서 기준으로 `ScrollUp`, `ScrollDown`, `ScrollLeft`, `ScrollRight` 이벤트를 명시적으로 처리하고, 터미널 앱이 마우스 모드를 소유한 경우에는 휠 이벤트를 PTY로 전달한다.

## 구현됨

### 런타임 안정성

- panic 후 터미널 상태 복구.
- 날짜/시간이 포함된 append-only crash log.
- 렌더링 전후 상태 보정: active terminal, split terminal, active editor tab, split editor, explorer selection, quick panel selection, prompt cursor index.
- crash log 기반 회귀 테스트와 짧은 라인 hover/휠 회귀 테스트.

### 셸 및 통합 터미널

- 실제 PTY 기반 셸 세션.
- 다중 터미널 탭, 클릭 전환, 생성, 종료, 재시작, 이름 변경, split terminal.
- vt100 기반 ANSI 색상/스타일 렌더링.
- 일반 키 입력, 수정키 조합, function key, bracketed paste, shell editing key, terminal mouse encoding 전달.
- 터미널 scrollback, 마우스 휠 스크롤, Shift-scroll host override, 텍스트 선택, 출력 복사, 검색, scroll-to-bottom.
- OSC 7 cwd 추적, OSC 0/2 title 추적, OSC52 clipboard 처리.
- 터미널 출력의 파일 참조와 URL 클릭 처리.

### 파일 익스플로러

- 실제 파일시스템 기반 트리, lazy directory loading, expand/collapse, 파일 열기, active file reveal, refresh.
- 마우스 hover, click, wheel, right-click context menu, drag/drop move, Alt-drag copy.
- 다중 선택, create file/folder, rename, delete confirmation, copy/cut/paste, duplicate, compare selected files, New Terminal Here.
- 이름, 타입, 수정 시간, 크기 기준 정렬.
- hidden/ignored/generated folder 표시 토글.
- Git working tree badge 표시.
- 파일 생성/삭제/rename prompt는 하단 status line이 아니라 상단 중앙 TUI dialog로 표시.

### 에디터

- 라인 번호, syntax highlighting, dirty marker, read-only binary/non-UTF-8/large-file preview가 있는 editable tab buffer.
- Untitled editor와 Save As.
- undo/redo, paste, selection, multi-cursor, multi-occurrence selection, word movement, line command, line/block comment, trailing whitespace trim, auto-pairing, smart indentation.
- find/replace, replace all, go to line, horizontal scroll, word wrap, mouse coordinate mapping.
- code folding, fold all, unfold all, gutter fold click, bookmark.
- matching bracket navigation, editor navigation history.
- split editor pane과 독립 스크롤.
- Open Editors panel.
- 열려 있는 파일의 외부 디스크 변경 감지.

### 워크스페이스와 코드 인텔리전스

- Quick Open, workspace search, replace in files, workspace symbols, document symbols, references, fallback definition lookup.
- 선택적 one-shot stdio LSP 지원: hover, signature help, definition, type definition, implementation, call hierarchy, document highlight, references, rename, completion, document symbols, workspace symbols, formatting, code actions, diagnostics.
- Problems panel과 editor gutter/status diagnostics.
- Source Control panel: Git changes, diff hunks, diff tabs, branch list/create/checkout, stage/unstage, commit staged, commit all, discard.
- `.vscode/tasks.json`, `package.json`, Cargo, Make, Go, Python project task detection.

### UI와 입력

- 마우스 우선 panel focus, hover highlight, wheel scrolling, tab switching, context menu, prompt dialog, quick panel.
- explorer, editor, workspace, Git, terminal command palette.
- 핵심 조작의 keyboard fallback.
- top title bar와 bottom status bar.

## 아직 구현되지 않았거나 부족함

### 런타임 안정성

- crash log를 앱 안에서 열어 보여주는 viewer가 없다.
- 장시간 soak test와 scripted mouse/keyboard/PTY 부하 테스트가 부족하다.
- 프로세스 종료나 crash 이후 session restore가 없다.

### 터미널 호환성

- vt100 기반 렌더링이라 xterm/kitty/iTerm2 전체 호환을 보장하지 않는다.
- nested TUI, alternate screen, bracketed paste edge case, advanced private mode 검증이 더 필요하다.
- terminal profile selector, environment editor, shell integration UI, terminal process tree가 없다.
- terminal tab drag reordering이 없다.

### 익스플로러 호환성

- VS Code식 복합 파일 작업 확인 modal, overwrite option, recursive conflict option이 없다.
- 파일 감시는 native filesystem notification이 아니라 polling/snapshot 기반이다.
- inline rename/create widget이 없다.
- symbolic link target inspector와 advanced permission editor가 없다.

### 에디터 호환성

- minimap, breadcrumbs, sticky scroll, inline diff, peek definition, peek references, inline rename widget이 없다.
- TextMate grammar/theme ecosystem 전체와 theme selection UI가 없다.
- settings UI, keybindings UI, color theme UI, font/language behavior 설정 UI가 없다.
- multi-root workspace가 없다.
- large file editing은 streaming editable chunk가 아니라 read-only preview로 제한한다.

### 언어 기능

- LSP는 long-lived synchronized server가 아니라 명령별 one-shot 방식이다.
- semantic tokens, inlay hints, code lens, LSP folding ranges, workspace diagnostics streaming, background indexing이 없다.
- completion은 inline suggest widget이 아니라 quick-panel picker다.
- Debug Adapter Protocol, breakpoint, variables, watch, call stack, debug console이 없다.

### Source Control과 확장성

- side-by-side diff review, merge conflict resolution UI, stash, remote sync, blame, timeline, PR integration이 없다.
- extension host, extension marketplace, plugin API, VS Code settings/keybinding compatibility layer가 없다.

### 릴리스와 설치

- 현재 prerelease는 의도적으로 `aarch64-apple-darwin`만 빌드한다.
- Linux `.deb`/`.rpm`, Windows, Raspberry Pi, macOS x86_64 산출물은 전체 매트릭스 릴리스 지시가 있을 때 복구한다.
- one-line installer는 유지되어 있지만, Apple Silicon 전용 prerelease 기간에는 다른 플랫폼 사용자가 예전 full-matrix prerelease를 써야 할 수 있다.
