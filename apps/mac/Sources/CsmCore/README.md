Populated at build time by `apps/mac/build-xcframework.sh`, which drops
`csm_core.swift` (UniFFI-generated Swift bindings) here. Do not commit
`csm_core.swift` — it regenerates from `crates/core-ffi` on every build.

This directory exists in git only to keep the `CsmCore` target's source path
valid for `xcodegen generate`.
