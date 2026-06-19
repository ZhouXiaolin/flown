# Flown

Flown is a coding agent runtime and interface where agent behavior, user interface surfaces, and session flow can be extended without changing the core application.

## Language

**Extension**:
A user- or application-provided capability that can register commands, tools, hooks, and runtime behaviors for the coding agent.
_Avoid_: plugin, addon

**Runtime Capability**:
A host-owned authority object handed to an extension so it can drive selected runtime behavior without owning internal application state.
_Avoid_: raw UI handle, direct state access

**Extension Context**:
The unified context object an extension uses to access host capabilities. Its surface is grouped by capability area rather than exposing raw runtime state.
_Avoid_: global app handle, service locator

**Command Proxy**:
A thread-safe extension-facing entry point that forwards UI or conversation actions back to the owning runtime thread.
_Avoid_: direct UI mutation, raw callback sink

**Hook**:
A lifecycle callback an extension registers to observe or selectively intercept host events.
_Avoid_: event bus listener, middleware

**Conversation Layer**:
A visible conversation surface with its own transcript and agent interaction lifecycle. The main conversation is one layer; extension-opened side conversations are additional layers.
_Avoid_: screen, page, tab

**Session**:
A single agent conversation instance with one attached UI surface, or no UI in silent mode. It is the unit that can be forked, switched, or replaced.
_Avoid_: workspace, app instance, terminal
