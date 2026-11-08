# Tokio overview

Tokio is an asynchronous runtime for the Rust programming language. It provides
the building blocks needed for writing network applications with async/await.

## Key components

- The multi-threaded, work-stealing task scheduler.
- Async TCP and UDP sockets.
- Timers and the `tokio::time` module.
- Utilities like `tokio::sync::mpsc` channels.
