# `/btw` Extension — Design Spec

- **Status**: Draft (2026-06-17)
- **Related**: ADR-0001 (extension arch), ADR-0002 (M3 btw intent), ADR-0003
  (M2 refinement — this work pulls the session-ops it deferred forward).
- **Convention**: Solaren. Prose for humans in Chinese; identifiers stay English.

## 1. Goal

`/btw` — a temporary, in-process side conversation that **forks the main
session's current history** into a transient copy, runs **concurrently** with
the main conversation (neither blocks the other), and is **discarded on exit**.

Two invocation forms (the slash-command registration concern this work starts
from):

1. `/btw`            → enter an empty btw transcript; wait for input.
2. `/btw <message>`  → enter btw transcript **and** submit `<message>`,
   kicking off a turn immediately.

Exit: **Ctrl+C** in a btw layer drops it and returns to the main view.

## 2. Confirmed semantics (from brainstorming)

| Question | Answer |
|---|---|
| Concurrency model | **Concurrent.** btw and main may each run agent turns at the same time; neither blocks the other. |
| Session relationship | **Copy snapshot.** btw gets its own harness seeded with a fork of the main session's current message history. The main session is never mutated by btw. |
| Entry timing | **Anytime.** `/btw` can be entered whether the main conversation is busy or idle. |
| Transcript view | **Full switch.** While in btw the transcript area shows only the btw conversation; main is hidden. Exiting switches back. |
| Exit handling | **Discard.** No prompt, no save. Drop the btw harness + UiState. |
| Ctrl+C semantics | In a btw layer, Ctrl+C = exit btw (not quit the app). |

These together force: **an independent `AgentHarness` per btw layer, an
independent flume channel + event pump per layer, and an independent
`UiState` per layer.** This is ADR-0002's M3 "btw (temporary, discarded)"
verbatim.

## 3. What we take from pi-mono (the essence)

Studied pi-mono's extension system (`packages/coding-agent/src/core/extensions/`
+ 60+ examples). Three findings shape this design:

### 3.1 Command handlers receive a *stateful context*, not a pure effect

pi-mono `registerCommand(name, { handler: async (args, ctx) => {...} })`. The
handler gets `ExtensionCommandContext` with `newSession()` / `fork()` /
`navigateTree()` / `switchSession()` / `waitForIdle()` / `ui.notify()` /
`sessionManager`. The handler **operates on `ctx` directly**; the runner binds
those methods to host-injected implementations via `bindCommandContext(actions)`
(`runner.ts:381`, `createCommandContext()` `runner.ts:688`).

This is the opposite of flown's current `CommandEffect` (pure `Send` data
returned by a handler). flown chose effect-as-data to dodge the `Rc<UiState>`
can't-cross-threads constraint. That trade-off is correct for **read-only
notify-style** commands (`/mcp list`) but cannot express "enter btw, then
optionally send a message" — that is sequential logic over live runtime state.

**Decision:** adopt pi-mono's pattern for commands that need to *drive* the
runtime. Introduce an iodilos-side `Rc<RuntimeControl>` (NOT `Send` — it lives
on the iodilos thread) injected into `CommandSide`, exposed to handlers that
opt in. `/btw` uses it; `/mcp` keeps returning `CommandEffect` (no change).

### 3.2 `withSession` callback — stale-ctx guard after session replacement

pi-mono marks the old `ctx` stale after `newSession`/`fork`/`switchSession`
(`runner.ts:511`: "This extension ctx is stale after session replacement …
move post-replacement work into `withSession`"). We mirror the *spirit*: after
`enter_btw`, the active layer changes; code that captured the old layer must
not keep poking it. Our `ConversationStack` makes this structurally enforced —
handlers operate via `RuntimeControl`, which always targets `active()`, so a
captured `Rc<UiState>` is simply not used post-switch.

### 3.3 btw is flown-original — no pi-mono precedent

pi-mono's `subagent` (`examples/extensions/subagent/index.ts`) spawns a
**separate `pi` process** (`--no-session`), not an in-process multi-harness.
pi-mono has **no transient / discardable side-conversation** concept — all its
sessions are persistent. This confirms ADR-0002: btw is flown-original design
with no port target. The pi-mono lessons we *do* borrow are the ctx-injection
mechanics (3.1) and the stale-guard discipline (3.2), not any btw feature.

## 4. Architecture

### 4.1 The conversation stack (iodilos side)

A new `Rc<ConversationStack>` replaces the single `Rc<UiState>` as the thing
components read. It is provided via iodilos context at mount.

```text
ConversationStack (Rc, iodilos context)
├── layers: Vec<Rc<ConversationLayer>>   // [0] = main (never popped)
├── active_index: RwSignal<usize>        // which layer the UI shows
└── runtime_control: Rc<RuntimeControl>  // injected into CommandSide

ConversationLayer (Rc)
├── state: Rc<UiState>                   // independent transcript
├── harness: Option<Arc<AgentHarness>>   // None in session-only mode
├── event_tx: flume::Sender<HarnessEvent>// independent channel
├── unsubscribe: Option<Box<dyn Fn()>>   // harness.subscribe teardown
├── btw_factory: Option<Rc<BtwFactory>>  // how to spawn a child btw
└── kind: LayerKind { Main | Btw { prompt } }
```

Components change **one** line: `use_context::<Rc<UiState>>()` →
`use_context::<Rc<ConversationStack>>().active().state`. A helper
`ConversationStack::active() -> Rc<ConversationLayer>` keeps this ergonomic.

The busy-spinner `every` tick, the event pump, and `on_key` all read
`stack.active()` so they always act on the visible layer.

### 4.2 `RuntimeControl` — the iodilos-side capability handle

```rust
pub struct RuntimeControl {
    stack: Rc<ConversationStack>,
    config: Config,
}

impl RuntimeControl {
    /// Enter a btw layer. Forks the active session's history into a transient
    /// in-memory session, builds an independent harness, wires its own event
    /// pump, pushes the layer, and optionally submits `prompt`.
    pub fn enter_btw(&self, prompt: Option<String>);

    /// Exit the active btw layer (aborts its harness, drops it, pops).
    /// No-op when active is Main.
    pub fn exit_btw(&self);

    /// Submit `text` as a user turn on the active layer's harness.
    pub fn send_to_active(&self, text: String);

    /// Read-only notify, for parity with the CommandEffect path.
    pub fn notify_active(&self, text: String);
    pub fn notify_error_active(&self, text: String);
    pub fn clear_active(&self);

    pub fn active_is_btw(&self) -> bool;
}
```

`RuntimeControl` is `Rc`-held, never crosses threads. `CommandSide` stores an
`Option<Rc<RuntimeControl>>`; `/btw`'s handler calls into it.

### 4.3 `BtwFactory` — the tokio-side harness builder

`enter_btw` runs on iodilos but must build a harness (async). The recipe is
captured once at bootstrap in a `BtwFactory` and cloned into each layer:

```rust
pub struct BtwFactory {
    model: Model,                 // Clone
    env: Arc<dyn ExecutionEnv>,   // shared
    built_in_tools: Vec<AgentTool>,
    system_prompt: String,
    api_key_fn: GetApiKeyAndHeadersFn,   // = Arc<dyn Fn(&Model) ->
                                         //   Option<(String, Option<HashMap<String,String>>)>
                                         //   + Send + Sync>, cloned from main harness
    // source session snapshotter (reads the MAIN harness's session branch)
    main_harness: Arc<AgentHarness>,
}
```

`enter_btw` flow (cross-thread):

```text
RuntimeControl.enter_btw(prompt)             [iodilos thread]
 ├─ create a fresh flume channel (event_tx, event_rx) now (sync)
 ├─ spawn_local an async bridge that:
 │    1. snapshot entries = main_harness.session().get_branch(None).await
 │       (this .await must run on tokio → wrap via a tokio::spawn)
 │    2. the tokio task: InMemorySessionStorage + copy_branch_to(entries),
 │       build new AgentHarness (same recipe as build_agent, in-memory fork
 │       as session), subscribe it → forward each HarnessEvent into event_tx,
 │       return (Arc<harness>, unsubscribe_token) via a flume build channel
 │    3. back on iodilos: build_rx.recv_async().await → (harness, unsub)
 │       a. new Rc<UiState>
 │       b. spawn_local event pump: event_rx.recv_async() → btw state
 │          (translate_event, same fn as main)
 │       c. ConversationLayer { harness, event_tx, unsubscribe: unsub, … }
 │          stack.push(layer); active_index → btw
 │       d. if prompt: tokio::spawn(harness.prompt(prompt)); busy=true
 └─ (return immediately; the user sees an empty btw transcript, the fork
     completes a few ms later and, if a prompt was given, streaming begins)
```

Two flume channels are in play, keep them distinct: (1) the **event channel**
(`event_tx`/`event_rx`) carrying `HarnessEvent`s from the btw harness to its
pump — long-lived for the layer's lifetime; (2) the **build channel**
(`flume::unbounded`, single send) carrying the built `(harness, unsubscribe)`
from the tokio builder task back to the iodilos bridge. The build channel is
single-use: the tokio task sends once, the bridge `recv_async().await`s once,
then both ends drop. Using flume (not a second channel type) for the build
channel means the bridge reuses the **same `recv_async().await`-on-`spawn_local`
pattern** the existing event pump already proves (runtime.rs:216) — one bridge
primitive for both channels.

**Async-on-iodilos bridge:** iodilos's `spawn_local` executor polls futures.
The bridge is `spawn_local(async move { let (h, unsub) =
build_rx.recv_async().await; ...finish wiring... })` — flume's `recv_async`
is pollable by iodilos's executor (this is exactly what the existing event
pump does at `runtime.rs:216`). No new threading primitive is introduced; both
the build channel and the event channel are `flume::unbounded`.

### 4.4 `exit_btw` flow

```text
RuntimeControl.exit_btw()                    [iodilos thread]
 ├─ guard: stack.active().kind must be Btw
 ├─ stack.pop() → layer; active_index → main   // UI switches back immediately
 ├─ (layer.unsubscribe)();                      // detach harness subscriber FIRST:
 │                                              //   harness stops emitting into event_tx
 ├─ if let Some(h) = &layer.harness:
 │    tokio::spawn(async { h.abort().await });  // stop in-flight turn (best-effort)
 ├─ drop(layer.event_tx)                        // pump's recv_async → Err → task exits
 └─ drop(layer)                                 // this Arc clone drops; in-memory
                                                //   session GC'd when last Arc goes
                                                //   (the spawned prompt task may hold one)
```

**Teardown order is load-bearing:** `unsubscribe` must run *before*
`drop(event_tx)`. Otherwise the harness could emit into a channel whose only
receiver (the pump) has already been dropped — flume tolerates this (unbounded
send never errors), but it's wasted work and muddies lifecycle reasoning. With
unsubscribe first, the harness stops emitting; then dropping `event_tx` lets
the pump observe "all senders gone" and exit.

### 4.5 Event routing — natural isolation

Each layer owns its own flume channel and its own `spawn_local` pump. A
harness is subscribed to **exactly one** channel (its layer's). There is no
source-tag multiplexing: main events go to the main pump → main UiState; btw
events go to the btw pump → btw UiState. This is why we chose independent
channels over a tagged single channel — it makes a wrong-routing bug
impossible by construction and makes exit a plain `drop`.

## 5. `/btw` command registration

`/btw` is a new extension command, but it needs `RuntimeControl` (not
`CommandEffect`). Two options were considered:

- **A) New `CommandEffect` variants** (`EnterBtw`/`ExitBtw`/`SendBtw`). Rejected:
  the effect enum would grow per-feature, and "enter then send" is sequential
  logic over live state — awkward as data.
- **B) `RuntimeControl` injection for opted-in commands.** Chosen.

### 5.1 `CommandHandler` gains an optional control handle

The handler type stays `Send + Sync` for the `/mcp` path (effect-only). For
commands that need runtime control, the iodilos-side dispatcher hands them a
`&RuntimeControl`. Concretely, `CommandSide::dispatch` becomes:

```rust
pub fn dispatch(&self, text: &str) -> bool {
    let Some((cmd, args)) = self.resolve(text) else { return false; };
    let effect = (cmd.handler)(&args);            // existing path (Notify/…)
    self.apply(effect);
    // AND, if the command opted into control, the handler already called
    // into the injected RuntimeControl before returning. See 5.2.
    true
}
```

Two handler shapes coexist (hybrid model, per brainstorming decision):

- **Effect handlers** (`/mcp`): `Arc<dyn Fn(&str) -> CommandEffect + Send + Sync>`.
  Capture only owned/Clone data; return an effect. Unchanged.
- **Control handlers** (`/btw`): registered with a reference to
  `RuntimeControl`. Because `RuntimeControl` is `Rc` (not `Send`), these
  handlers are wired **on the iodilos side at mount** (in the same closure
  that builds `CommandSide`), not during `register()` on tokio. The tokio-side
  `register()` records only the command *metadata* (name/description); the
  handler body is attached at bind time.

This split mirrors pi-mono: registration collects metadata; the host binds
live capability at runtime (`bindCommandContext`).

### 5.2 `BtwExtension`

```rust
pub struct BtwExtension { /* config only, for metadata */ }

impl Extension for BtwExtension {
    fn register(&self, api: &mut ExtensionApi) {
        api.register_command("/btw", CommandMeta::simple(
            "Open a temporary side conversation (forks current history, Ctrl+C to exit)"
        ), /* control-handler placeholder */);
    }
}
```

Because the `/btw` handler needs `RuntimeControl` (iodilos-only), `BtwExtension`
registers **metadata only**. The actual handler is installed at mount by
`CommandSide` wiring (a `ControlCommand` entry carrying a
`fn(&str, &RuntimeControl)`). Parsing is trivial:

```text
/btw            → enter_btw(None)
/btw <message>  → enter_btw(Some(message.to_string()))
```

(`/btw` has no subcommands, so no second-level popup.)

### 5.3 `app.rs` dispatch order (unchanged shape, new capability)

`handle_app_key` already does extension-dispatch-first. The only addition: in
a btw layer, **Ctrl+C routes to `exit_btw`** instead of the existing
"clear-input / quit" logic. Concretely, the Ctrl-C branch gains a guard at the
top:

```text
if ctrl_c && stack.active_is_btw() && input_empty {
    runtime_control.exit_btw();
    return true;  // consumed
}
// …existing ctrl-c logic for main layer…
```

## 6. McpExtension corrections (done in the same change)

Studying pi-mono surfaced two issues in the current McpExtension worth fixing
while we touch the extension layer:

### 6.1 Dead `ToolHandle` is misleading

`register_mcp_tools` takes a `ToolHandle` then drops it (`mcp.rs:94` "dropped
intentionally"). The comment claims a "wiring-layer watcher" will take its own
handle later — but no such watcher exists, and `register()` is the only place
that can mint a handle for this extension's store. Taking-then-dropping reads
as a TODO, not a design. **Fix:** remove the dead `tool_handle()` call. If a
runtime watcher is ever needed, it will be added then; until then the one-shot
registration is the whole story and shouldn't pretend otherwise.

### 6.2 `/mcp status` lies about live state

`mcp_status_text` only reads `config.mcp_servers` (disabled/enabled), then
admits "Run `flown mcp status` for live connection info." With
`RuntimeControl`/`CommandSide` now having access to richer runtime state, and
since the `McpManager` (when present) knows live connection state, `/mcp
status` should report **actual** connection state, not a config echo. **Fix:**
thread the live `Arc<Mutex<McpManager>>` (already captured by the extension)
into the status handler so `/mcp status` shows connected/disconnected per
server. This is a small, contained improvement that makes the command honest.

(Both fixes stay within `mcp.rs`; neither changes the extension trait or the
effect path. `/mcp` remains effect-only — it does **not** adopt
`RuntimeControl`.)

## 7. Files touched

| File | Change |
|---|---|
| `core/extensions/types.rs` | Add `ControlCommand`/control-handler shape; doc the hybrid model. No change to existing `CommandEffect`/`CommandHandler`. |
| `core/extensions/runner.rs` | `CommandSide` holds `Option<Rc<RuntimeControl>>`; `dispatch` routes control-commands to it. |
| `core/extensions/mod.rs` | `build_runner` adds `BtwExtension`; wires control-commands at build. |
| **NEW** `core/extensions/btw.rs` | `BtwExtension` + `enter_btw`/`exit_btw` logic + `BtwFactory`. |
| **NEW** `tui/conversation.rs` | `ConversationStack`, `ConversationLayer`, `RuntimeControl`. |
| `tui/runtime.rs` | Mount builds `ConversationStack` instead of bare `UiState`; main layer wired as today; `RuntimeControl` provided via context + injected into `CommandSide`. |
| `tui/components/app.rs` | `use_context` reads `ConversationStack`; `on_key` operates on `stack.active()`; Ctrl-C in btw → `exit_btw`. |
| `tui/components/{transcript,status_line,editor,hint_bar}.rs` | Each swaps `Rc<UiState>` → `Rc<ConversationStack>` (read `active().state`). Mechanical. |
| `tui/slash_commands.rs` | No change (static commands unaffected). |
| `core/extensions/mcp.rs` | 6.1 (drop dead handle) + 6.2 (live `/mcp status`). |

## 8. Threading & lifecycle invariants (the load-bearing constraints)

1. **`RuntimeControl` never crosses threads.** It is `Rc`, built and used only
   in the iodilos mount closure and `on_key`. Handlers that need it are wired
   at mount, not in `register()` (which runs on tokio).
2. **`BtwFactory` is `Send`** (all fields `Arc`/`Clone`/`String`). It is built
   on tokio at bootstrap and cloned into `ConversationStack` (which itself
   lives on iodilos — the factory is inert data until `enter_btw` spawns a
   tokio task that uses it).
3. **Each layer = one harness + one channel + one pump.** A harness is
   subscribed to exactly its layer's channel. Exit = unsubscribe + drop sender
   + pop. No source-tag routing exists.
4. **`abort` before drop.** `exit_btw` spawns `harness.abort()` so an in-flight
   turn stops cleanly; the harness Arc may still be held by the spawned
   `prompt` future until it observes abort. Dropping the layer's `Arc` clone is
   fine — the spawned task holds its own.
5. **Main layer is never popped.** `exit_btw` guards on `active_is_btw()`.
6. **Session-only mode** (no harness): `BtwFactory` is `None`; `/btw` reports
   "No LLM agent available" via `notify_error_active`, mirroring the existing
   no-agent behavior. The stack still works (main layer has `harness: None`).

## 9. Testing strategy

- **Unit (no TUI):**
  - `RuntimeControl` enter/exit over a `ConversationStack` with a fake/stub
    harness — assert push/pop, active_index transitions, active_is_btw.
  - `/btw` arg parse: `None` vs `Some("msg")`.
  - McpExtension 6.1/6.2 fixes: assert no dead handle; `/mcp status` reflects
    a mocked live manager.
- **Integration (flume + translate_event):** build a real main layer + a btw
  layer whose harness is an in-memory fork; push a prompt; assert events land
  in the btw UiState, not the main one (the isolation guarantee). Then
  `exit_btw`; assert main is active and the btw channel is closed.
- **Manual smoke:** `/btw`, type, submit, watch streaming in the btw view;
  Ctrl+C returns to main with main's transcript intact and main able to keep
  running its own turn.

## 10. Out of scope

- Nesting btw-within-btw (`/btw` while already in btw). The stack supports it
  structurally, but the Ctrl-C "exit one level" vs "exit all" UX is deferred.
  v1: `/btw` while in btw is rejected with a notify ("already in a btw; exit
  first"). The stack depth-1 guard is one line.
- Persistent btw (saving a btw to the session tree). That's **Fork**
  (ADR-0005), a separate feature. btw stays discardable.
- btw result carry-back ("bring an answer into main"). Deferred; exit is
  pure-discard in v1.
- `/mcp` adopting `RuntimeControl`. It stays effect-only; the 6.x fixes don't
  require it.
