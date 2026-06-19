---
status: accepted
---

# Async ExtensionContext with command-proxy capabilities

We will expose a unified `ExtensionContext` to in-process Rust extensions, but implement it as a capability-backed facade rather than a raw state handle. Extension commands are async and return `Result`, while UI, conversation, and session actions go through a host-owned command proxy so extensions can drive runtime behavior without owning internal state across thread boundaries.
