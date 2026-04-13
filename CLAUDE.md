# Tracking

* Store PRDs and tasks in linear under the "Claude Session Monitor" project within the "Projects" team

# Architecture

Cargo workspace: `crates/{common,server,reporter,gui}`.

## Common crate design

- Organise modules vertically by functionality, not by code structure
- Each module exposes a small, stable interface to decouple modules from each other
- Tests at module boundaries only, no unit tests
